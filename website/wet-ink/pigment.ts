// The guest supplies the moving pigment field. This module only samples and paints it.
const vertex = `#version 300 es
out vec2 uv;
void main() {
  vec2 p = vec2((gl_VertexID << 1) & 2, gl_VertexID & 2);
  uv = vec2(p.x, 1.0 - p.y);
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}`;
const fragment = `#version 300 es
precision highp float;
uniform sampler2D pigments;
uniform vec2 dimensions;
in vec2 uv;
out vec4 color;
vec2 pigmentAt(vec2 coord) {
  vec2 texel = coord * dimensions - .5;
  vec2 k = floor(texel);
  vec2 f = fract(texel);
  vec2 f2 = f * f;
  vec2 f3 = f2 * f;
  vec2 d = 1.0 - f;
  vec2 w0 = d * d * d * (1.0 / 6.0);
  vec2 w1 = (4.0 - 6.0 * f2 + 3.0 * f3) * (1.0 / 6.0);
  vec2 w2 = (1.0 + 3.0 * f + 3.0 * f2 - 3.0 * f3) * (1.0 / 6.0);
  vec2 w3 = f3 * (1.0 / 6.0);
  vec2 a = w0 + w1;
  vec2 b = w2 + w3;
  vec2 at0 = (k - .5 + w1 / a) / dimensions;
  vec2 at1 = (k + 1.5 + w3 / b) / dimensions;
  vec2 top = texture(pigments, at0).rg * a.x
      + texture(pigments, vec2(at1.x, at0.y)).rg * b.x;
  vec2 bottom = texture(pigments, vec2(at0.x, at1.y)).rg * a.x
      + texture(pigments, at1).rg * b.x;
  return top * a.y + bottom * b.y;
}
void main() {
  vec2 p = pigmentAt(uv);
  vec3 paper = vec3(250, 238, 225) / 255.0;
  vec3 veil = vec3(242, 215, 192) / 255.0;
  vec3 brick = vec3(170, 53, 37) / 255.0;
  vec3 port = vec3(92, 45, 53) / 255.0;
  vec3 ink = vec3(49, 30, 33) / 255.0;
  float red = smoothstep(.457, .479, p.r);
  float edge0 = .292 + .145 * smoothstep(.29, .75, p.g);
  float underprint = smoothstep(edge0, edge0 + .026, p.r);
  float dark = smoothstep(.470, .502, p.g);
  float body = smoothstep(.620, .655, p.g);
  vec3 flood = mix(brick, mix(port, ink, body), dark);
  vec3 c = mix(mix(paper, veil, underprint), flood, red);
  color = vec4(c, 1.0);
}`;

function shader(gl: WebGL2RenderingContext, stage: number, source: string) {
  const result = gl.createShader(stage);
  if (!result) throw new Error('Could not create the Wet Ink shader.');
  gl.shaderSource(result, source);
  gl.compileShader(result);
  if (!gl.getShaderParameter(result, gl.COMPILE_STATUS))
    throw new Error(gl.getShaderInfoLog(result) ?? 'Could not compile the Wet Ink shader.');
  return result;
}

export interface PigmentPainter {
  kind: 'canvas2d' | 'webgl2' | 'webgl2-software';
  resize(): void;
  paint(pigments: Uint8Array): void;
}

function resizeCanvas(canvas: HTMLCanvasElement, limit: number) {
  const box = canvas.getBoundingClientRect();
  const width = Math.max(1, Math.round(box.width * devicePixelRatio));
  const height = Math.max(1, Math.round(box.height * devicePixelRatio));
  const scale = Math.min(1, limit / width, (limit * 9) / (height * 16));
  const nextWidth = Math.max(1, Math.round(width * scale));
  const nextHeight = Math.max(1, Math.round(height * scale));
  if (canvas.width !== nextWidth) canvas.width = nextWidth;
  if (canvas.height !== nextHeight) canvas.height = nextHeight;
}

export function createPigmentPainter(
  canvas: HTMLCanvasElement,
  width: number,
  height: number,
): PigmentPainter {
  canvas.width = width;
  canvas.height = height;
  const gl = canvas.getContext('webgl2', {
    alpha: false,
    antialias: false,
    preserveDrawingBuffer: true,
  });
  if (!gl) {
    const ctx = canvas.getContext('2d', { alpha: false });
    const tile = document.createElement('canvas');
    tile.width = width;
    tile.height = height;
    const tex = tile.getContext('2d');
    if (!ctx || !tex) throw new Error('Could not create the Wet Ink canvas.');
    const image = tex.createImageData(width, height);
    const colors = new Uint32Array(image.data.buffer);
    const tone = (edge0: number, edge1: number, value: number) => {
      const t = Math.min(1, Math.max(0, (value - edge0) / (edge1 - edge0)));
      return t * t * (3 - 2 * t);
    };
    const channel = (
      paper: number,
      veil: number,
      brick: number,
      port: number,
      ink: number,
      underprint: number,
      a: number,
      dark: number,
      body: number,
    ) => {
      const ground = paper + (veil - paper) * underprint;
      const fill = brick * (1 - dark) + dark * (port + (ink - port) * body);
      return Math.round(ground + (fill - ground) * a);
    };
    const palette = new Uint32Array(65536);
    for (let i = 0; i < palette.length; i++) {
      const red = (i & 255) / 255,
        ink = (i >> 8) / 255;
      const edge0 = 0.292 + 0.145 * tone(0.29, 0.75, ink);
      const a = tone(0.457, 0.479, red),
        underprint = tone(edge0, edge0 + 0.026, red);
      const dark = tone(0.47, 0.502, ink),
        body = tone(0.62, 0.655, ink);
      const r = channel(250, 242, 170, 92, 49, underprint, a, dark, body);
      const g = channel(238, 215, 53, 45, 30, underprint, a, dark, body);
      const b0 = channel(225, 192, 37, 53, 33, underprint, a, dark, body);
      palette[i] = 0xff000000 | (b0 << 16) | (g << 8) | r;
    }
    return {
      kind: 'canvas2d',
      resize() {
        resizeCanvas(canvas, 1920);
        ctx.imageSmoothingQuality = 'high';
      },
      paint(pigments) {
        const words = new Uint16Array(pigments.buffer, pigments.byteOffset, width * height);
        // The views have equal lengths and the palette covers every 16-bit word.
        for (let i = 0; i < colors.length; i++) colors[i] = palette[words[i]!]!;
        tex.putImageData(image, 0, 0);
        ctx.drawImage(tile, 0, 0, canvas.width, canvas.height);
      },
    };
  }
  const debug = gl.getExtension('WEBGL_debug_renderer_info');
  const renderer: unknown = debug ? gl.getParameter(debug.UNMASKED_RENDERER_WEBGL) : '';
  const software =
    typeof renderer === 'string' && /SwiftShader|llvmpipe|software rasterizer/i.test(renderer);
  const program = gl.createProgram();
  if (!program) throw new Error('Could not create the Wet Ink graphics program.');
  gl.attachShader(program, shader(gl, gl.VERTEX_SHADER, vertex));
  gl.attachShader(program, shader(gl, gl.FRAGMENT_SHADER, fragment));
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS))
    throw new Error(
      gl.getProgramInfoLog(program) ?? 'Could not link the Wet Ink graphics program.',
    );
  gl.useProgram(program);
  gl.uniform2f(gl.getUniformLocation(program, 'dimensions'), width, height);
  const texture = gl.createTexture();
  if (!texture) throw new Error('Could not create the Wet Ink texture.');
  gl.bindTexture(gl.TEXTURE_2D, texture);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.REPEAT);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.REPEAT);
  gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
  gl.texStorage2D(gl.TEXTURE_2D, 1, gl.RG8, width, height);
  gl.disable(gl.DITHER);
  return {
    kind: software ? 'webgl2-software' : 'webgl2',
    resize() {
      // Software rasterizers run the four-tap shader on the CPU.
      resizeCanvas(canvas, software ? 1280 : 3840);
      gl.viewport(0, 0, gl.drawingBufferWidth, gl.drawingBufferHeight);
    },
    paint(pigments) {
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, width, height, gl.RG, gl.UNSIGNED_BYTE, pigments);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
    },
  };
}
