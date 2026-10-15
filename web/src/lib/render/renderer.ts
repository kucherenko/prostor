// The WebGL2 compositor: executes a `RenderPlan` on an OffscreenCanvas. Only textured
// quads: the camera (the latest `VideoFrame`), text (rasterised once per text into a
// texture) and solid colours. WebGL2 rather than a 2D canvas because spike 0001 found
// that GPU-backed 2D-canvas frames starve the hardware encoder from a Worker.

import type { RenderLayer } from '#lib/generated/RenderLayer';
import type { RenderPlan } from '#lib/generated/RenderPlan';
import { sourceUv, toClip } from './geometry';

const VERTEX_SHADER = `#version 300 es
in vec2 a_pos;
uniform vec4 u_rect;
uniform vec4 u_uv;
out vec2 v_uv;
void main() {
	vec2 p = vec2(u_rect.x + a_pos.x * u_rect.z, u_rect.y - a_pos.y * u_rect.w);
	gl_Position = vec4(p, 0.0, 1.0);
	v_uv = mix(u_uv.xy, u_uv.zw, a_pos);
}`;

const FRAGMENT_SHADER = `#version 300 es
precision mediump float;
in vec2 v_uv;
uniform sampler2D u_tex;
uniform vec4 u_color;
uniform float u_textured;
uniform float u_opacity;
out vec4 o_color;
void main() {
	vec4 c = mix(u_color, texture(u_tex, v_uv), u_textured);
	o_color = vec4(c.rgb, c.a * u_opacity);
}`;

interface TextTexture {
	key: string;
	texture: WebGLTexture;
	width: number;
	height: number;
}

export class Renderer {
	readonly canvas: OffscreenCanvas;
	private readonly gl: WebGL2RenderingContext;
	private readonly uniforms: {
		rect: WebGLUniformLocation;
		uv: WebGLUniformLocation;
		color: WebGLUniformLocation;
		textured: WebGLUniformLocation;
		opacity: WebGLUniformLocation;
	};
	private plan: RenderPlan;
	private readonly cameraTexture: WebGLTexture;
	private cameraFrame: VideoFrame | null = null;
	private cameraUploaded = false;
	private cameraSize: [number, number] = [0, 0];
	private readonly textTextures = new Map<string, TextTexture>();
	private textCanvas: OffscreenCanvas | null = null;

	constructor(plan: RenderPlan) {
		this.plan = plan;
		this.canvas = new OffscreenCanvas(plan.width, plan.height);
		const gl = this.canvas.getContext('webgl2', {
			alpha: false,
			antialias: false,
			depth: false,
			stencil: false,
			preserveDrawingBuffer: false,
			premultipliedAlpha: true
		});
		if (!gl) throw new Error('WebGL2 is not available in this Worker');
		this.gl = gl;
		const program = link(gl, VERTEX_SHADER, FRAGMENT_SHADER);
		gl.useProgram(program);
		const uniform = (name: string) => {
			const location = gl.getUniformLocation(program, name);
			if (!location) throw new Error(`missing uniform ${name}`);
			return location;
		};
		this.uniforms = {
			rect: uniform('u_rect'),
			uv: uniform('u_uv'),
			color: uniform('u_color'),
			textured: uniform('u_textured'),
			opacity: uniform('u_opacity')
		};
		gl.uniform1i(uniform('u_tex'), 0);

		const vao = gl.createVertexArray();
		gl.bindVertexArray(vao);
		const buffer = gl.createBuffer();
		gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
		gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([0, 0, 1, 0, 0, 1, 1, 1]), gl.STATIC_DRAW);
		const position = gl.getAttribLocation(program, 'a_pos');
		gl.enableVertexAttribArray(position);
		gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 0, 0);

		gl.enable(gl.BLEND);
		gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
		gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, true);
		this.cameraTexture = createTexture(gl);
	}

	get width(): number {
		return this.plan.width;
	}

	get height(): number {
		return this.plan.height;
	}

	setPlan(plan: RenderPlan): void {
		this.plan = plan;
		if (this.canvas.width !== plan.width || this.canvas.height !== plan.height) {
			this.canvas.width = plan.width;
			this.canvas.height = plan.height;
		}
	}

	/** Takes ownership of the frame; the previous one is closed. */
	setCameraFrame(frame: VideoFrame): void {
		this.cameraFrame?.close();
		this.cameraFrame = frame;
		this.cameraUploaded = false;
	}

	/** Draws the plan into the canvas. Returns false when nothing could be drawn. */
	draw(): boolean {
		const gl = this.gl;
		const { width, height } = this.plan;
		gl.viewport(0, 0, width, height);
		gl.clearColor(0, 0, 0, 1);
		gl.clear(gl.COLOR_BUFFER_BIT);
		for (const layer of this.plan.layers) {
			if (layer.rect.w <= 0 || layer.rect.h <= 0 || layer.opacity <= 0) continue;
			this.drawLayer(layer);
		}
		return true;
	}

	private drawLayer(layer: RenderLayer) {
		const gl = this.gl;
		const { width, height } = this.plan;
		const [x, y, w, h] = toClip(layer.rect, width, height);
		gl.uniform4f(this.uniforms.rect, x, y, w, h);
		gl.uniform1f(this.uniforms.opacity, layer.opacity);
		switch (layer.kind.type) {
			case 'camera': {
				if (!this.uploadCamera()) return;
				const [u0, v0, u1, v1] = sourceUv(
					this.cameraSize[0],
					this.cameraSize[1],
					layer.rect.w,
					layer.rect.h,
					layer.crop
				);
				gl.uniform4f(this.uniforms.uv, u0, v0, u1, v1);
				gl.uniform1f(this.uniforms.textured, 1);
				break;
			}
			case 'text': {
				const text = this.textTexture(layer.kind.text, layer.rect.w, layer.rect.h);
				gl.bindTexture(gl.TEXTURE_2D, text.texture);
				gl.uniform4f(this.uniforms.uv, 0, 0, 1, 1);
				gl.uniform1f(this.uniforms.textured, 1);
				break;
			}
			case 'color': {
				const [r, g, b, a] = layer.kind.rgba;
				// Premultiplied, to match the blend function.
				const alpha = a / 255;
				gl.uniform4f(
					this.uniforms.color,
					(r / 255) * alpha,
					(g / 255) * alpha,
					(b / 255) * alpha,
					alpha
				);
				gl.uniform1f(this.uniforms.textured, 0);
				break;
			}
			default:
				// Screen share, images and guests arrive in later milestones: a dark slot.
				gl.uniform4f(this.uniforms.color, 0.12, 0.12, 0.14, 1);
				gl.uniform1f(this.uniforms.textured, 0);
		}
		gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
	}

	private uploadCamera(): boolean {
		const gl = this.gl;
		const frame = this.cameraFrame;
		if (!frame) return false;
		gl.bindTexture(gl.TEXTURE_2D, this.cameraTexture);
		if (!this.cameraUploaded) {
			gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, frame);
			this.cameraSize = [frame.displayWidth, frame.displayHeight];
			this.cameraUploaded = true;
		}
		return true;
	}

	private textTexture(text: string, w: number, h: number): TextTexture {
		const width = Math.max(1, Math.round(w));
		const height = Math.max(1, Math.round(h));
		const key = `${width}x${height}:${text}`;
		const cached = this.textTextures.get(key);
		if (cached) return cached;
		const gl = this.gl;
		this.textCanvas ??= new OffscreenCanvas(width, height);
		const canvas = this.textCanvas;
		canvas.width = width;
		canvas.height = height;
		const ctx = canvas.getContext('2d');
		if (!ctx) throw new Error('2D context for text rendering is unavailable');
		ctx.clearRect(0, 0, width, height);
		ctx.font = `bold ${Math.round(height * 0.6)}px system-ui, sans-serif`;
		ctx.textBaseline = 'middle';
		ctx.fillStyle = 'rgba(0, 0, 0, 0.55)';
		ctx.fillRect(0, 0, width, height);
		ctx.fillStyle = '#ffffff';
		ctx.fillText(text, Math.round(height * 0.3), height / 2, width - height * 0.6);
		const texture = createTexture(gl);
		gl.bindTexture(gl.TEXTURE_2D, texture);
		gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, canvas);
		const entry = { key, texture, width, height };
		// A small cache: the scene changes rarely, but never let it grow without bound.
		if (this.textTextures.size >= 32) {
			const [oldest] = this.textTextures;
			if (oldest) {
				gl.deleteTexture(oldest[1].texture);
				this.textTextures.delete(oldest[0]);
			}
		}
		this.textTextures.set(key, entry);
		return entry;
	}

	/** Releases the camera frame and GPU resources. */
	dispose(): void {
		this.cameraFrame?.close();
		this.cameraFrame = null;
		const gl = this.gl;
		for (const entry of this.textTextures.values()) gl.deleteTexture(entry.texture);
		this.textTextures.clear();
		gl.deleteTexture(this.cameraTexture);
	}
}

function createTexture(gl: WebGL2RenderingContext): WebGLTexture {
	const texture = gl.createTexture();
	if (!texture) throw new Error('cannot create a texture');
	gl.bindTexture(gl.TEXTURE_2D, texture);
	gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
	gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
	gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
	gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
	return texture;
}

function link(gl: WebGL2RenderingContext, vertex: string, fragment: string): WebGLProgram {
	const compile = (type: number, source: string) => {
		const shader = gl.createShader(type);
		if (!shader) throw new Error('cannot create a shader');
		gl.shaderSource(shader, source);
		gl.compileShader(shader);
		if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
			throw new Error(`shader: ${gl.getShaderInfoLog(shader) ?? 'compile error'}`);
		}
		return shader;
	};
	const program = gl.createProgram();
	if (!program) throw new Error('cannot create a program');
	gl.attachShader(program, compile(gl.VERTEX_SHADER, vertex));
	gl.attachShader(program, compile(gl.FRAGMENT_SHADER, fragment));
	gl.linkProgram(program);
	if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
		throw new Error(`program: ${gl.getProgramInfoLog(program) ?? 'link error'}`);
	}
	return program;
}
