/**
 * WebGPU waveform painter. One instanced quad per pixel column, expanded in
 * the vertex shader from a per-column (x, min, max) instance record. A
 * second instanced pass draws the dimmer full-peak silhouette behind a
 * brighter pseudo-RMS core, matching the Canvas2D look.
 */

import type { WaveformPainter } from "./painter";

const SHADER = /* wgsl */ `
struct Uniforms {
  viewport : vec2f,   // device px
  color    : vec4f,   // premultiplied-ready straight alpha
  ampScale : f32,     // 1.0 for peaks pass, 0.55 for core pass
  alphaMul : f32,
  _pad     : vec2f,
};
@group(0) @binding(0) var<uniform> u : Uniforms;

struct VSOut {
  @builtin(position) pos : vec4f,
};

@vertex
fn vs(
  @builtin(vertex_index) vi : u32,
  @location(0) column : vec3f,   // x(device px), min(-1..1), max(-1..1)
) -> VSOut {
  let corner = vec2f(f32(vi & 1u), f32(vi >> 1u)); // (0,0)(1,0)(0,1)(1,1)
  let xPx = column.x + corner.x;                    // 1-px wide quad
  let lo = clamp(column.y * u.ampScale, -1.0, 1.0);
  let hi = clamp(column.z * u.ampScale, -1.0, 1.0);
  // guarantee at least a hairline
  let minSpan = 2.0 / u.viewport.y;
  let span = max(hi - lo, minSpan);
  let centre = (hi + lo) * 0.5;
  let yNorm = centre - span * 0.5 + span * corner.y; // -1..1, +1 = top
  var out : VSOut;
  out.pos = vec4f((xPx / u.viewport.x) * 2.0 - 1.0, yNorm, 0.0, 1.0);
  return out;
}

@fragment
fn fs() -> @location(0) vec4f {
  let a = u.color.a * u.alphaMul;
  return vec4f(u.color.rgb * a, a); // premultiplied
}
`;

const UNIFORM_SIZE = 48; // 2f viewport + pad-to-16 handled via explicit layout

let sharedDevice: GPUDevice | null = null;
let devicePromise: Promise<GPUDevice | null> | null = null;

async function getDevice(): Promise<GPUDevice | null> {
  if (sharedDevice) return sharedDevice;
  if (!devicePromise) {
    devicePromise = (async () => {
      const gpu = navigator.gpu;
      if (!gpu) return null;
      try {
        const adapter = await gpu.requestAdapter();
        if (!adapter) return null;
        const device = await adapter.requestDevice();
        device.lost.then(() => {
          sharedDevice = null;
          devicePromise = null;
        });
        sharedDevice = device;
        return device;
      } catch {
        return null;
      }
    })();
  }
  return devicePromise;
}

export async function webGpuAvailable(): Promise<boolean> {
  return (await getDevice()) !== null;
}

export async function createWebGpuPainter(canvas: HTMLCanvasElement): Promise<WaveformPainter> {
  const device = await getDevice();
  if (!device) throw new Error("no WebGPU device");
  const gpu = navigator.gpu!;
  const context = canvas.getContext("webgpu") as GPUCanvasContext | null;
  if (!context) throw new Error("no webgpu canvas context");
  const format = gpu.getPreferredCanvasFormat();
  context.configure({ device, format, alphaMode: "premultiplied" });

  const module = device.createShaderModule({ code: SHADER, label: "aura-waveform" });
  const pipeline = device.createRenderPipeline({
    label: "aura-waveform",
    layout: "auto",
    vertex: {
      module,
      entryPoint: "vs",
      buffers: [
        {
          arrayStride: 12,
          stepMode: "instance",
          attributes: [{ shaderLocation: 0, offset: 0, format: "float32x3" }],
        },
      ],
    },
    fragment: {
      module,
      entryPoint: "fs",
      targets: [
        {
          format,
          blend: {
            color: { srcFactor: "one", dstFactor: "one-minus-src-alpha", operation: "add" },
            alpha: { srcFactor: "one", dstFactor: "one-minus-src-alpha", operation: "add" },
          },
        },
      ],
    },
    primitive: { topology: "triangle-strip" },
  });

  let instanceBuf: GPUBuffer | null = null;
  let instanceCapacity = 0;
  // two uniform buffers: peaks pass + core pass
  const uniforms = [0, 1].map(() =>
    device.createBuffer({
      size: UNIFORM_SIZE,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
      label: "aura-waveform-uniforms",
    }),
  );
  const bindGroups = uniforms.map((buf) =>
    device.createBindGroup({
      layout: pipeline.getBindGroupLayout(0),
      entries: [{ binding: 0, resource: { buffer: buf } }],
    }),
  );

  let destroyed = false;

  return {
    kind: "webgpu",

    draw(columns, cssWidth, cssHeight, dpr, color) {
      if (destroyed) return;
      const pw = Math.max(1, Math.round(cssWidth * dpr));
      const ph = Math.max(1, Math.round(cssHeight * dpr));
      if (canvas.width !== pw || canvas.height !== ph) {
        canvas.width = pw;
        canvas.height = ph;
      }

      const cols = Math.min(cssWidth, columns.length / 2);
      const instances = Math.max(0, Math.floor(cols * dpr));
      if (instances === 0) return;

      // Build device-pixel instance records by sampling CSS columns.
      const data = new Float32Array(instances * 3);
      for (let i = 0; i < instances; i++) {
        const srcX = Math.min(cols - 1, Math.floor(i / dpr));
        data[i * 3] = i;
        data[i * 3 + 1] = columns[srcX * 2];
        data[i * 3 + 2] = columns[srcX * 2 + 1];
      }
      if (!instanceBuf || instanceCapacity < data.byteLength) {
        instanceBuf?.destroy();
        instanceCapacity = Math.max(data.byteLength, 4096);
        instanceBuf = device.createBuffer({
          size: instanceCapacity,
          usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
          label: "aura-waveform-instances",
        });
      }
      device.queue.writeBuffer(instanceBuf, 0, data);

      const writeUniform = (index: number, ampScale: number, alphaMul: number) => {
        const u = new Float32Array(UNIFORM_SIZE / 4);
        u[0] = pw;
        u[1] = ph;
        u[4] = color.r;
        u[5] = color.g;
        u[6] = color.b;
        u[7] = color.a;
        u[8] = ampScale;
        u[9] = alphaMul;
        device.queue.writeBuffer(uniforms[index], 0, u);
      };
      writeUniform(0, 1.0, 0.55); // full peak silhouette, dimmer
      writeUniform(1, 0.55, 0.85); // core band, brighter

      const encoder = device.createCommandEncoder();
      const pass = encoder.beginRenderPass({
        colorAttachments: [
          {
            view: context.getCurrentTexture().createView(),
            clearValue: { r: 0, g: 0, b: 0, a: 0 },
            loadOp: "clear",
            storeOp: "store",
          },
        ],
      });
      pass.setPipeline(pipeline);
      pass.setVertexBuffer(0, instanceBuf);
      pass.setBindGroup(0, bindGroups[0]);
      pass.draw(4, instances);
      pass.setBindGroup(0, bindGroups[1]);
      pass.draw(4, instances);
      pass.end();
      device.queue.submit([encoder.finish()]);
    },

    destroy() {
      destroyed = true;
      instanceBuf?.destroy();
      for (const buf of uniforms) buf.destroy();
      try {
        context.unconfigure();
      } catch {
        /* context may already be gone */
      }
    },
  };
}
