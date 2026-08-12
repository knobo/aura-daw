/**
 * Minimal ambient WebGPU declarations — just the slice of the API the
 * waveform renderer uses. TypeScript's DOM lib does not ship WebGPU types
 * and adding @webgpu/types would be a new dependency (forbidden here).
 */

interface GPUObjectBase {
  label?: string;
}

interface GPU {
  requestAdapter(options?: { powerPreference?: "low-power" | "high-performance" }): Promise<GPUAdapter | null>;
  getPreferredCanvasFormat(): string;
}

interface GPUAdapter {
  requestDevice(descriptor?: Record<string, unknown>): Promise<GPUDevice>;
}

interface GPUDevice extends GPUObjectBase {
  queue: GPUQueue;
  lost: Promise<{ reason: string; message: string }>;
  createShaderModule(descriptor: { code: string; label?: string }): GPUShaderModule;
  createBuffer(descriptor: { size: number; usage: number; label?: string; mappedAtCreation?: boolean }): GPUBuffer;
  createRenderPipeline(descriptor: Record<string, unknown>): GPURenderPipeline;
  createBindGroup(descriptor: Record<string, unknown>): GPUBindGroup;
  createCommandEncoder(descriptor?: { label?: string }): GPUCommandEncoder;
  destroy(): void;
}

interface GPUQueue {
  writeBuffer(buffer: GPUBuffer, offset: number, data: BufferSource, dataOffset?: number, size?: number): void;
  submit(buffers: GPUCommandBuffer[]): void;
}

interface GPUShaderModule extends GPUObjectBase {}

interface GPUBuffer extends GPUObjectBase {
  size: number;
  destroy(): void;
}

interface GPURenderPipeline extends GPUObjectBase {
  getBindGroupLayout(index: number): unknown;
}

interface GPUBindGroup extends GPUObjectBase {}

interface GPUCommandBuffer extends GPUObjectBase {}

interface GPUCommandEncoder extends GPUObjectBase {
  beginRenderPass(descriptor: Record<string, unknown>): GPURenderPassEncoder;
  finish(descriptor?: { label?: string }): GPUCommandBuffer;
}

interface GPURenderPassEncoder {
  setPipeline(pipeline: GPURenderPipeline): void;
  setBindGroup(index: number, bindGroup: GPUBindGroup): void;
  setVertexBuffer(slot: number, buffer: GPUBuffer): void;
  draw(vertexCount: number, instanceCount?: number, firstVertex?: number, firstInstance?: number): void;
  end(): void;
}

interface GPUTexture extends GPUObjectBase {
  createView(): GPUTextureView;
}

interface GPUTextureView extends GPUObjectBase {}

interface GPUCanvasContext {
  configure(configuration: { device: GPUDevice; format: string; alphaMode?: "opaque" | "premultiplied" }): void;
  unconfigure(): void;
  getCurrentTexture(): GPUTexture;
}

interface Navigator {
  readonly gpu?: GPU;
}

declare const GPUBufferUsage: {
  VERTEX: number;
  UNIFORM: number;
  COPY_DST: number;
};
