//! Cranelift kernels for the track strip.
//!
//! ## What is actually being fused, and why it is worth doing
//!
//! `mixer::apply_fader_into` runs one loop per track per block, and inside that
//! loop, per sample, it:
//!
//! 1. asks a [`crate::automation::RampCursor`] for the gain — a `while` over a
//!    slice, an `Option`, and a division inside the interpolation;
//! 2. lerps the pan — including `i as f32 / last as f32`, **a divide per
//!    sample**;
//! 3. branches on mute;
//! 4. multiplies, folds two peaks and two sums, and adds into the output.
//!
//! Steps 1–3 produce, over any stretch between two automation breakpoints,
//! nothing but a straight line. [`crate::strip::plan`] finds those stretches
//! and states the line's two coefficients; what is left for the inner loop is
//! four multiplies and three adds with no branch, no lookup and no divide —
//! which is also the first shape in this pipeline that can be put in a vector
//! register.
//!
//! So the kernel is not "the same loop, JIT-compiled". It is a different loop
//! that the plan made possible, and this module's job is to emit it as SSE
//! code that handles two frames at a time.
//!
//! ## Why a JIT rather than a few `#[inline]` functions
//!
//! For the two shapes here, honestly: not much. Both could be const-generic
//! Rust, and [`crate::dsp::fused_scalar`] is deliberately in the benchmark to
//! show exactly how much of the win is the plan rather than the code
//! generator. The reason to build the JIT seam anyway is the shape space
//! *past* the fader — an insert chain is an arbitrary-length list of nodes
//! chosen at runtime, and fusing `[gain, ramp, pan]` is the smallest honest
//! rehearsal of fusing `[eq, comp, gain, ramp, pan]` without writing a
//! function per permutation. The claim this module supports is "the seam works
//! and costs nothing at runtime", not "cranelift beats LLVM".
//!
//! ## The RT contract
//!
//! Compilation happens **once**, on the control thread, in [`Kernels::compile`]
//! — never on demand from a callback. What reaches the audio thread is a
//! function pointer. [`Kernels::run`] allocates nothing, and the executable
//! pages outlive every call by construction (see [`Kernels::release`] for the
//! one thing that is *not* automatic).

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::Endianness;
use cranelift_codegen::ir::{types, AbiParam, InstBuilder, MemFlagsData, UserFuncName, Value};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{default_libcall_names, Linkage, Module};

use crate::dsp::Accum;
use crate::strip::{Coef, Plan};

/// The compiled kernel's ABI.
///
/// * `inp` — interleaved stereo input, `frames` frames.
/// * `outp` — the post-fader buffer at the stretch's first frame; the kernel
///   **overwrites** it. Not `mixer::mix_out`'s accumulate: since Plan G2 the
///   fader's destination is a per-strip stereo buffer that routing reads
///   afterwards, so `emit` stores rather than read-modify-writes, and
///   `equivalence.rs` asserts the overwrite. A caller wiring a raw
///   [`KernelFn`] against the old wording would double-mix.
/// * `frames` — must be even; the odd tail frame is finished in Rust (one
///   frame is not worth a second code path, and doing it in Rust keeps it
///   bit-identical to the scalar reference).
/// * `coefp` — `[g0, dg, gl0, dgl, gr0, dgr]`, i.e. [`Coef`] in declaration
///   order.
/// * `accp` — 8 floats: four peak lanes then four sum-of-squares lanes, in
///   the buffer's own interleaving (lanes 0/2 are left, 1/3 are right).
///   Read-modify-write, so consecutive stretches accumulate.
pub type KernelFn = unsafe extern "C" fn(
    inp: *const f32,
    outp: *mut f32,
    frames: i64,
    coefp: *const f32,
    accp: *mut f32,
);

/// Which kernel a stretch needs. The whole specialisation, in one bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Constant gain and pan: no index arithmetic at all.
    Flat,
    /// Gain and/or pan moving: one affine step per vector.
    Affine,
}

impl Shape {
    pub fn of(coef: &Coef) -> Self {
        if coef.is_flat() {
            Shape::Flat
        } else {
            Shape::Affine
        }
    }
}

#[derive(Debug)]
pub enum JitError {
    Host(String),
    Codegen(String),
}

impl std::fmt::Display for JitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JitError::Host(m) => write!(f, "host is not supported by cranelift: {m}"),
            JitError::Codegen(m) => write!(f, "kernel codegen failed: {m}"),
        }
    }
}

impl std::error::Error for JitError {}

/// The compiled kernel table, and the module whose pages hold it.
///
/// Compiled once and then read-only. `Send` so it can be handed to the audio
/// thread — by a [`crate::sync::triple_buffer`], which is what keeps the
/// *drop* on the control thread; see [`Kernels::release`].
pub struct Kernels {
    flat: KernelFn,
    affine: KernelFn,
    module: JITModule,
}

// SAFETY: after `compile` returns, nothing mutates the module or the pages;
// the two function pointers are into finalized, read-execute memory. Send is
// what lets the table be published to the audio thread; it is deliberately
// NOT Sync — one reader, exactly as the triple buffer provides.
unsafe impl Send for Kernels {}

impl Kernels {
    /// Compile every kernel shape. **Control thread only** — this maps pages,
    /// runs a register allocator and takes milliseconds.
    pub fn compile() -> Result<Self, JitError> {
        let mut flag_builder = settings::builder();
        // Both settings are what the cranelift-jit example uses for a
        // straightforwardly-callable native function; neither is a
        // performance knob.
        flag_builder.set("use_colocated_libcalls", "false").map_err(cg)?;
        flag_builder.set("is_pic", "false").map_err(cg)?;
        // `speed` over `speed_and_size`: this is a hot inner loop and there is
        // one of it. Nothing here is cold.
        flag_builder.set("opt_level", "speed").map_err(cg)?;
        let isa_builder =
            cranelift_native::builder().map_err(|m| JitError::Host(m.to_string()))?;
        let isa = isa_builder.finish(settings::Flags::new(flag_builder)).map_err(cg)?;
        let mut module = JITModule::new(JITBuilder::with_isa(isa, default_libcall_names()));

        let mut sig = module.make_signature();
        let ptr = module.target_config().pointer_type();
        sig.params.push(AbiParam::new(ptr)); // inp
        sig.params.push(AbiParam::new(ptr)); // outp
        sig.params.push(AbiParam::new(types::I64)); // frames
        sig.params.push(AbiParam::new(ptr)); // coefp
        sig.params.push(AbiParam::new(ptr)); // accp

        let mut ctx = module.make_context();
        let mut fb_ctx = FunctionBuilderContext::new();
        let mut ids = Vec::new();
        for (name, shape) in [("strip_flat", Shape::Flat), ("strip_affine", Shape::Affine)] {
            let id = module.declare_function(name, Linkage::Export, &sig).map_err(cg)?;
            ctx.func.signature = sig.clone();
            ctx.func.name = UserFuncName::user(0, id.as_u32());
            emit(&mut ctx.func, &mut fb_ctx, ptr, shape, module.target_config());
            module.define_function(id, &mut ctx).map_err(cg)?;
            module.clear_context(&mut ctx);
            ids.push(id);
        }
        module.finalize_definitions().map_err(cg)?;

        // SAFETY: the pointers come from `get_finalized_function` on ids
        // declared with the signature `KernelFn` describes, and the module is
        // moved into `Self` so the pages outlive every call.
        let flat = unsafe { std::mem::transmute::<*const u8, KernelFn>(
            module.get_finalized_function(ids[0]),
        ) };
        let affine = unsafe { std::mem::transmute::<*const u8, KernelFn>(
            module.get_finalized_function(ids[1]),
        ) };
        Ok(Self { flat, affine, module })
    }

    pub fn kernel(&self, shape: Shape) -> KernelFn {
        match shape {
            Shape::Flat => self.flat,
            Shape::Affine => self.affine,
        }
    }

    /// Whether this table can run `plan` at all.
    ///
    /// One case falls back to [`crate::dsp::apply_fader_into`], and it is the
    /// caller's business rather than an error: `plan.overflowed`, meaning more
    /// automation breakpoints inside the block than the plan holds.
    ///
    /// Output channel count used to be a second case. It stopped being one
    /// when Plan G2 split the strip: the fader writes contiguous stereo into a
    /// post-fader buffer, and the master's channel count is
    /// `mixer::mix_post_into`'s problem now.
    pub fn can_run(plan: &Plan) -> bool {
        !plan.overflowed
    }

    /// Run a plan into a post-fader buffer. Audio thread. No allocation, no
    /// locking, no branching per sample.
    ///
    /// `post` must hold the whole run (`frames * 2` samples) — the mixer
    /// slices it to `run * 2` before calling, and so must every caller here,
    /// because a muted strip clears exactly what it is given.
    ///
    /// Returns false when [`Self::can_run`] says this plan is not for the
    /// kernels; the caller must then use the scalar path. A caller that
    /// ignored the return value would leave the previous run's audio in
    /// `post`, which routing would then send to the master.
    #[must_use = "false means the run was NOT rendered; fall back to the scalar path"]
    pub fn run(&self, plan: &Plan, buf: &[f32], post: &mut [f32], acc: &mut Accum) -> bool {
        if !Self::can_run(plan) {
            return false;
        }
        if plan.silent {
            // Clear, do not skip: `post` is an overwrite target that the sends
            // and the routing read after the fader.
            post.fill(0.0);
            return true;
        }
        for seg in plan.segments() {
            let c = seg.coef;
            let coef = [c.g0, c.dg, c.gl0, c.dgl, c.gr0, c.dgr];
            let even = seg.frames & !1;
            if even > 0 {
                let mut lanes = [0.0f32; 8];
                let inp = &buf[seg.offset * 2..(seg.offset + even) * 2];
                let outp = &mut post[seg.offset * 2..(seg.offset + even) * 2];
                let f = self.kernel(Shape::of(&c));
                // SAFETY: `inp` and `outp` are slices of exactly `even`
                // frames — sliced above, so a wrong length is a panic here
                // rather than a stray write inside the kernel. `coef` is 6
                // floats and `lanes` is 8, matching `KernelFn`'s contract, and
                // `even` is even by construction.
                unsafe {
                    f(
                        inp.as_ptr(),
                        outp.as_mut_ptr(),
                        even as i64,
                        coef.as_ptr(),
                        lanes.as_mut_ptr(),
                    );
                }
                // Reduce the four vector lanes into the two channels. Lanes
                // 0/2 are left, 1/3 right — the buffer's own interleaving.
                acc.pk_l = acc.pk_l.max(lanes[0]).max(lanes[2]);
                acc.pk_r = acc.pk_r.max(lanes[1]).max(lanes[3]);
                acc.ss_l += lanes[4] + lanes[6];
                acc.ss_r += lanes[5] + lanes[7];
            }
            // The odd tail frame, in Rust: one frame does not pay for a
            // second code path, and this way it is bit-identical to
            // `dsp::fused_scalar`'s arithmetic.
            if even < seg.frames {
                let i = even;
                let fi = i as f32;
                let g = c.g0 + c.dg * fi;
                let frame = seg.offset + i;
                let l = buf[frame * 2] * g * (c.gl0 + c.dgl * fi);
                let r = buf[frame * 2 + 1] * g * (c.gr0 + c.dgr * fi);
                acc.fold(l, r);
                post[frame * 2] = l;
                post[frame * 2 + 1] = r;
            }
        }
        true
    }

    /// Unmap the executable pages.
    ///
    /// Not `Drop`, and that is cranelift's decision rather than this crate's:
    /// `JITModule::free_memory` is `unsafe` because nothing can prove no
    /// compiled code is still executing or reachable. Dropping a `Kernels`
    /// therefore **leaks its pages** — a few KiB, once per compile.
    ///
    /// Which is precisely the reclamation point a triple buffer gives you: the
    /// writer that publishes a replacement table is handed the previous one
    /// back, on the control thread, after the reader has moved on. That is the
    /// moment at which the safety condition below is satisfiable at all.
    ///
    /// # Safety
    ///
    /// No kernel from this table may be running or be reachable from any
    /// thread.
    pub unsafe fn release(self) {
        unsafe { self.module.free_memory() }
    }
}

/// `Debug` rather than `Display` deliberately: cranelift's `CodegenError`
/// displays a verifier failure as the two words "Verifier errors" and keeps
/// the list — the only part that says WHICH instruction is wrong — in its
/// `Debug`. A codegen bug you cannot read is a codegen bug you cannot fix.
fn cg<E: std::fmt::Debug>(e: E) -> JitError {
    JitError::Codegen(format!("{e:?}"))
}

/// Emit one kernel.
///
/// Layout, for `Shape::Affine` (the flat kernel is this with every `d*` term
/// dropped at codegen time rather than multiplied by zero at runtime):
///
/// ```text
/// gv   = [g0, g0, g0, g0]      + [dg, dg, dg, dg]     * iv
/// pv   = [gl0, gr0, gl0, gr0]  + [dgl, dgr, dgl, dgr] * iv
/// x    = load  [l_i, r_i, l_i+1, r_i+1]
/// prod = (x * gv) * pv                 // same order as `apply_fader_into`
/// store  prod                          // post-fader buffer: overwrite
/// pk   = max(pk, |prod|)  ;  ss += prod * prod
/// iv  += [2, 2, 2, 2]     ;  i  += 2
/// ```
///
/// `iv` carries the frame index as a float, `[i, i, i+1, i+1]`. Stepping it by
/// 2.0 is exact for every block size a device will ever ask for (integers
/// below 2^24 are exactly representable and adding 2.0 to one stays exact), so
/// the vector lane holds precisely `i as f32` — which is what makes the kernel
/// bit-identical to [`crate::dsp::fused_scalar`] and not merely close to it.
fn emit(
    func: &mut cranelift_codegen::ir::Function,
    fb_ctx: &mut FunctionBuilderContext,
    ptr: types::Type,
    shape: Shape,
    target: cranelift_codegen::isa::TargetFrontendConfig,
) {
    let f32x4 = types::F32X4;
    // Not `MemFlagsData::trusted()`: that claims natural alignment, and a `&[f32]`
    // is only 4-byte aligned. `notrap` alone is what is true here.
    let flags = MemFlagsData::new().with_notrap();

    let mut b = FunctionBuilder::new(func, fb_ctx);
    let entry = b.create_block();
    let header = b.create_block();
    let body = b.create_block();
    let exit = b.create_block();

    b.switch_to_block(entry);
    b.append_block_params_for_function_params(entry);
    let inp = b.block_params(entry)[0];
    let outp = b.block_params(entry)[1];
    let frames = b.block_params(entry)[2];
    let coefp = b.block_params(entry)[3];
    let accp = b.block_params(entry)[4];

    let ld = |b: &mut FunctionBuilder, at: i32| b.ins().load(types::F32, flags, coefp, at);
    let g0 = ld(&mut b, 0);
    let gl0 = ld(&mut b, 8);
    let gr0 = ld(&mut b, 16);

    // gv/pv bases. `insertlane` builds [gl0, gr0, gl0, gr0] from two splats.
    let gv_base = b.ins().splat(f32x4, g0);
    let pv_base = {
        let v = b.ins().splat(f32x4, gl0);
        let v = b.ins().insertlane(v, gr0, 1);
        b.ins().insertlane(v, gr0, 3)
    };
    let (dgv, dpv) = match shape {
        Shape::Flat => (None, None),
        Shape::Affine => {
            let dg = ld(&mut b, 4);
            let dgl = ld(&mut b, 12);
            let dgr = ld(&mut b, 20);
            let dgv = b.ins().splat(f32x4, dg);
            let dpv = {
                let v = b.ins().splat(f32x4, dgl);
                let v = b.ins().insertlane(v, dgr, 1);
                b.ins().insertlane(v, dgr, 3)
            };
            (Some(dgv), Some(dpv))
        }
    };

    // Loop-carried state. `declare_var` + `def_var`/`use_var` lets
    // cranelift-frontend insert the block parameters and phis; hand-rolling
    // them is how a codegen bug gets written.
    let v_i = b.declare_var(types::I64);
    let v_pk = b.declare_var(f32x4);
    let v_ss = b.declare_var(f32x4);
    let v_iv = b.declare_var(f32x4);

    let zero_i = b.ins().iconst(types::I64, 0);
    b.def_var(v_i, zero_i);
    let pk0 = b.ins().load(f32x4, flags, accp, 0);
    let ss0 = b.ins().load(f32x4, flags, accp, 16);
    b.def_var(v_pk, pk0);
    b.def_var(v_ss, ss0);
    if shape == Shape::Affine {
        // iv = [0, 0, 1, 1]
        let z = b.ins().f32const(0.0);
        let one = b.ins().f32const(1.0);
        let v = b.ins().splat(f32x4, z);
        let v = b.ins().insertlane(v, one, 2);
        let v = b.ins().insertlane(v, one, 3);
        b.def_var(v_iv, v);
    } else {
        // Never read, but every variable must be defined on every path into
        // the loop or the SSA builder inserts an undefined phi.
        let z = b.ins().f32const(0.0);
        let v = b.ins().splat(f32x4, z);
        b.def_var(v_iv, v);
    }
    // Round the frame count down to a whole number of vectors; the caller
    // finishes an odd frame itself.
    let even = b.ins().band_imm_s(frames, -2);
    b.ins().jump(header, &[]);

    b.switch_to_block(header);
    let i = b.use_var(v_i);
    let more = b.ins().icmp(IntCC::SignedLessThan, i, even);
    b.ins().brif(more, body, &[], exit, &[]);

    b.switch_to_block(body);
    let i = b.use_var(v_i);
    // 2 frames * 2 channels * 4 bytes = 16 bytes per iteration, so the byte
    // offset of frame `i` is `i * 8`.
    let byte = b.ins().imul_imm_s(i, 8);
    let byte = byte_as(&mut b, byte, ptr);
    let src = b.ins().iadd(inp, byte);
    let dst = b.ins().iadd(outp, byte);
    let x = b.ins().load(f32x4, flags, src, 0);
    let (gv, pv) = match shape {
        Shape::Flat => (gv_base, pv_base),
        Shape::Affine => {
            let iv = b.use_var(v_iv);
            let dgv = dgv.expect("affine shape loads dg");
            let dpv = dpv.expect("affine shape loads dgl/dgr");
            let step_g = b.ins().fmul(dgv, iv);
            let step_p = b.ins().fmul(dpv, iv);
            (b.ins().fadd(gv_base, step_g), b.ins().fadd(pv_base, step_p))
        }
    };
    // `(x * g) * pan` — the multiply order `apply_fader_into` uses. Reassociating
    // it would move the last bit and cost the flat case its bit-identity.
    let scaled = b.ins().fmul(x, gv);
    let prod = b.ins().fmul(scaled, pv);
    // Overwrite, not accumulate. Plan G2's post-fader buffer is what removed
    // the load half of a read-modify-write from every iteration of every
    // track — the mixer's own loop got the same saving.
    b.ins().store(flags, prod, dst, 0);

    let pk = b.use_var(v_pk);
    let mag = b.ins().fabs(prod);
    // `fmax` would be the obvious instruction and is the wrong one: cranelift
    // implements IEEE/WASM maximum, which has to handle NaN and signed zero,
    // and lowers to a multi-instruction sequence on x86. A compare-and-blend
    // is `cmpps` + `blendvps`, and for the non-NaN samples a peak meter deals
    // in it computes the same value. (A NaN in the buffer makes the peak
    // unreliable rather than the audio: the sample itself is already broken by
    // then, and `mixer::TrackAccum` is telemetry.)
    let bigger = b.ins().fcmp(FloatCC::GreaterThan, mag, pk);
    // `fcmp` on a float vector yields an INTEGER lane mask; `bitselect` wants
    // all three operands one type. The bitcast is free (it is the same
    // register), and `little` is the lane order both sides already agree on.
    let bigger = b.ins().bitcast(f32x4, MemFlagsData::new().with_endianness(Endianness::Little), bigger);
    let pk = b.ins().bitselect(bigger, mag, pk);
    b.def_var(v_pk, pk);
    let ss = b.use_var(v_ss);
    let sq = b.ins().fmul(prod, prod);
    let ss = b.ins().fadd(ss, sq);
    b.def_var(v_ss, ss);

    if shape == Shape::Affine {
        let iv = b.use_var(v_iv);
        let two = b.ins().f32const(2.0);
        let step = b.ins().splat(f32x4, two);
        let iv = b.ins().fadd(iv, step);
        b.def_var(v_iv, iv);
    }
    let i = b.use_var(v_i);
    let next = b.ins().iadd_imm_s(i, 2);
    b.def_var(v_i, next);
    b.ins().jump(header, &[]);

    b.switch_to_block(exit);
    let pk = b.use_var(v_pk);
    let ss = b.use_var(v_ss);
    b.ins().store(flags, pk, accp, 0);
    b.ins().store(flags, ss, accp, 16);
    b.ins().return_(&[]);

    b.seal_all_blocks();
    b.finalize(target);
}

/// Widen (or keep) an `I64` byte offset to the target's pointer width.
fn byte_as(b: &mut FunctionBuilder, v: Value, ptr: types::Type) -> Value {
    if ptr == types::I64 {
        v
    } else {
        b.ins().ireduce(ptr, v)
    }
}
