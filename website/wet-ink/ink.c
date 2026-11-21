#include <stdint.h>

#define TAU 6.28318530718f

enum {
    WIDTH = 192,
    HEIGHT = 108,
    AREA = WIDTH * HEIGHT,
    POINTER_BITS = 16,
    POINTER_SCALE = 1 << POINTER_BITS,
    POINTER_MASK = POINTER_SCALE - 1,
    SUBCELL_BITS = 6,
    SUBCELL_SCALE = 1 << SUBCELL_BITS,
    SUBCELL_MASK = SUBCELL_SCALE - 1,
    PIGMENT_BITS = 10,
    PIGMENT_MAX = (1 << PIGMENT_BITS) - 1,
    PIGMENT_LANE_BITS = 16,
};

_Static_assert(PIGMENT_BITS + SUBCELL_BITS <= PIGMENT_LANE_BITS,
               "dye interpolation must stay within each pigment lane");

typedef uint32_t Dye;
typedef struct { int16_t x, y; } Velocity;
typedef struct {
    // The worker reads interleaved red and dark bytes from the start of guest state.
    uint8_t pixels[2 * AREA];
    Dye dye[AREA];
    Dye transported[AREA];
    Velocity wake[AREA];
    Dye tighten[PIGMENT_MAX + 1];
    int initialRed, initialDark;
    int totalRed, totalDark;
} Ink;

static inline Dye pack_dye(unsigned red, unsigned dark) {
    return red | (dark << PIGMENT_LANE_BITS);
}

static inline unsigned dye_red(Dye dye) { return dye & PIGMENT_MAX; }
static inline unsigned dye_dark(Dye dye) { return dye >> PIGMENT_LANE_BITS; }

static inline void store_pixel(Ink *ink, int index, unsigned red, unsigned dark) {
    ink->pixels[2 * index] = (uint8_t)(red >> (PIGMENT_BITS - 8));
    ink->pixels[2 * index + 1] = (uint8_t)(dark >> (PIGMENT_BITS - 8));
}

static inline float min(float a, float b) { return a < b ? a : b; }
static inline float max(float a, float b) { return a > b ? a : b; }
static inline float clamp(float v, float lo, float hi) { return min(hi, max(lo, v)); }

static float sin_turns(float turns) {
    float t = turns - (float)(int)(turns + 32.5f) + 32.0f;
    if (t > .25f) t = .5f - t;
    if (t < -.25f) t = -.5f - t;
    float x = TAU * t;
    float xx = x * x;
    return x * (1.0f + xx * (-0.166655f + xx * (0.0083109f + xx * (-0.0001849f))));
}

static inline int wrap(int x, int count) { return x < 0 ? x + count : (x >= count ? x - count : x); }

static void seed(Ink *ink, unsigned originX, unsigned originY) {
    int dx = ((originX & POINTER_MASK) * WIDTH) >> POINTER_BITS;
    int dy = ((originY & POINTER_MASK) * HEIGHT) >> POINTER_BITS;
    ink->initialRed = ink->initialDark = 0;
    for (int y = 0; y < HEIGHT; y++) {
        int row = wrap(y + dy, HEIGHT) * WIDTH;
        for (int x = 0; x < WIDTH; x++) {
            float u = (float)x / WIDTH, v = (float)y / HEIGHT;
            float s = u + .18f * sin_turns(v + .03f) + .05f * sin_turns(v * 2.0f - u);
            float t = v + .23f * sin_turns(u + .13f) + .06f * sin_turns(u * 3.0f + v);
            float sheet = .83f * sin_turns(s + t) + .61f * sin_turns(2.0f * t - s + .22f)
                        + .35f * sin_turns(2.0f * s + t - .13f) + .16f * sin_turns(3.0f * s + 2.0f * t);
            float layer = .63f * sin_turns(2.0f * s - t + .05f) + .46f * sin_turns(s + 2.0f * t - .18f)
                        + .22f * sin_turns(3.0f * t + .30f);
            int i = row + wrap(x + dx, WIDTH);
            float red = .11f + .78f * clamp(.5f + .73f * sheet, 0.0f, 1.0f);
            float dark = .12f + .76f * clamp(.5f + .82f * layer, 0.0f, 1.0f);
            unsigned r = (unsigned)(red * PIGMENT_MAX), d = (unsigned)(dark * PIGMENT_MAX);
            ink->dye[i] = pack_dye(r, d);
            ink->initialRed += r;
            ink->initialDark += d;
            ink->wake[i] = (Velocity){0, 0};
        }
    }
    ink->totalRed = ink->initialRed;
    ink->totalDark = ink->initialDark;
}

typedef struct {
    float x, y;
    float stepX, stepY;
    int left, right, top, bottom;
} Brush;

static Brush brush_at(uint32_t px, uint32_t py, uint32_t previous, uint32_t active) {
    Brush brush = {0};
    if (!active) return brush;
    float lastX = (float)(previous & POINTER_MASK) * (WIDTH / (float)POINTER_SCALE);
    float lastY = (float)(previous >> POINTER_BITS) * (HEIGHT / (float)POINTER_SCALE);
    float x = (float)px * (WIDTH / (float)POINTER_SCALE);
    float y = (float)py * (HEIGHT / (float)POINTER_SCALE);
    float vx = x - lastX, vy = y - lastY;
    float speed2 = vx * vx + vy * vy;
    if (speed2 < .0001f) return brush;
    float response = 01.10f / (1.0f + .10f * __builtin_sqrtf(speed2));
    // Center the force between the last and current samples to cover quick gestures.
    brush.x = x - clamp(vx, -10.0f, 10.0f) * .44f;
    brush.y = y - clamp(vy, -10.0f, 10.0f) * .44f;
    brush.stepX = vx * response;
    brush.stepY = vy * response;
    brush.left = (int)max(0.0f, brush.x - 18.0f);
    brush.right = (int)min(WIDTH, brush.x + 19.0f);
    brush.top = (int)max(0.0f, brush.y - 18.0f);
    brush.bottom = (int)min(HEIGHT, brush.y + 19.0f);
    return brush;
}

static void push(Ink *ink, Brush brush) {
    for (int y = brush.top; y < brush.bottom; y++) {
        for (int x = brush.left; x < brush.right; x++) {
            float u = (float)x + .5f - brush.x, v = (float)y + .5f - brush.y;
            float q = max(0.0f, 1.0f - (u * u + v * v) * (1.0f / (18.0f * 18.0f)));
            float edge = min(min((float)x, WIDTH - (float)x - 1.0f),
                             min((float)y, HEIGHT - (float)y - 1.0f));
            float amount = q * q * min(1.0f, edge * .32f);
            // A compact curl gives the stroke a return flow around its path.
            float twist = (4.0f / (18.0f * 18.0f)) * (brush.stepX * v - brush.stepY * u);
            Velocity *wake = &ink->wake[y * WIDTH + x];
            wake->x = (int16_t)clamp(wake->x + 32.0f * amount * (q * brush.stepX - v * twist), -210, 210);
            wake->y = (int16_t)clamp(wake->y + 32.0f * amount * (q * brush.stepY + u * twist), -210, 210);
        }
    }
}

static inline Dye interpolate(Dye a, Dye b, unsigned weight) {
    const Dye rounding = pack_dye(SUBCELL_SCALE / 2, SUBCELL_SCALE / 2);
    const Dye fractions = pack_dye(SUBCELL_MASK, SUBCELL_MASK);
    const Dye weighted = (SUBCELL_SCALE - weight) * a + weight * b + rounding;
    // Clear both fractional parts before shifting the packed lanes together.
    return (weighted & ~fractions) >> SUBCELL_BITS;
}

static void sharpen(Ink *ink) {
    // Correct slow volume loss at the moving interfaces.
    const float gain = .66f / (AREA * (float)PIGMENT_MAX);
    const float redBias = clamp((ink->initialRed - ink->totalRed) * gain, -.019f, .019f);
    const float darkBias = clamp((ink->initialDark - ink->totalDark) * gain, -.019f, .019f);
    for (int i = 0; i <= PIGMENT_MAX; i++) {
        float v = (float)i / PIGMENT_MAX;
        float shape = .048f * (v - .5f);
        float strength = v * (1.0f - v);
        unsigned red = (unsigned)(clamp(v + strength * (shape + redBias), 0.0f, 1.0f) * PIGMENT_MAX + .5f);
        unsigned dark = (unsigned)(clamp(v + strength * (shape + darkBias), 0.0f, 1.0f) * PIGMENT_MAX + .5f);
        ink->tighten[i] = pack_dye(red, dark);
    }
}

static void advect(Ink *ink, uint32_t tick) {
    int row[HEIGHT], column[WIDTH];
    float time = (float)(tick & 32767) * (1.0f / 32768.0f);
    for (int y = 0; y < HEIGHT; y++) {
        float v = (float)y / HEIGHT;
        row[y] = (int)((SUBCELL_SCALE * .625f) * (.10f + .22f * sin_turns(v - 31.0f * time)
                               + .11f * sin_turns(2.0f * v + 13.0f * time)));
    }
    for (int x = 0; x < WIDTH; x++) {
        float u = (float)x / WIDTH;
        column[x] = (int)((SUBCELL_SCALE * .625f) * (.05f + .27f * sin_turns(u + 23.0f * time + .37f)
                                  + .09f * sin_turns(2.0f * u - 17.0f * time)));
    }
    const Dye *source = tick & 1 ? ink->dye : ink->transported;
    Dye *dest = tick & 1 ? ink->transported : ink->dye;
    sharpen(ink);
    int totalRed = 0, totalDark = 0;
    for (int y = 0; y < HEIGHT; y++) {
        for (int x = 0; x < WIDTH; x++) {
            int i = y * WIDTH + x;
            Velocity wake = ink->wake[i];
            ink->wake[i] = (Velocity){
                (int16_t)((wake.x * 57) / 64),
                (int16_t)((wake.y * 57) / 64),
            };
            int sx = x * SUBCELL_SCALE - row[y] - wake.x;
            int sy = y * SUBCELL_SCALE - column[x] - wake.y;
            int ox = sx >> SUBCELL_BITS, oy = sy >> SUBCELL_BITS;
            int l = wrap(ox, WIDTH), r = wrap(ox + 1, WIDTH);
            int n = wrap(oy, HEIGHT) * WIDTH, s = wrap(oy + 1, HEIGHT) * WIDTH;
            unsigned fx = (unsigned)sx & SUBCELL_MASK, fy = (unsigned)sy & SUBCELL_MASK;
            Dye a = interpolate(source[n + l], source[n + r], fx);
            Dye b = interpolate(source[s + l], source[s + r], fx);
            Dye color = interpolate(a, b, fy);
            unsigned red = dye_red(ink->tighten[dye_red(color)]);
            unsigned dark = dye_dark(ink->tighten[dye_dark(color)]);
            totalRed += red;
            totalDark += dark;
            dest[i] = pack_dye(red, dark);
            store_pixel(ink, i, red, dark);
        }
    }
    ink->totalRed = totalRed;
    ink->totalDark = totalDark;
}

// The ELF linker script places step at the start of the flat guest image.
#if defined(__ELF__)
__attribute__((section(".text.step")))
#endif
void step(Ink *ink, uint32_t tick, uint32_t x, uint32_t y, uint32_t previous, uint32_t active) {
    if (tick == 0) {
        // The first position places the dye before the brush starts moving it.
        seed(ink, x, y);
        for (int i = 0; i < AREA; i++)
            store_pixel(ink, i, dye_red(ink->dye[i]), dye_dark(ink->dye[i]));
    } else {
        push(ink, brush_at(x, y, previous, active));
        advect(ink, tick);
    }
}
