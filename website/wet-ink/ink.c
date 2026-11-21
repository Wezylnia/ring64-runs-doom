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
    BRUSH_RADIUS = 18,
    WAKE_LIMIT = 210,
    CURRENT_PERIOD = 32768,
};

_Static_assert(PIGMENT_BITS + SUBCELL_BITS <= PIGMENT_LANE_BITS,
               "dye interpolation must stay within each pigment lane");

typedef uint32_t Dye;
typedef struct {
    int16_t x, y;
} Velocity;

typedef struct {
    // The worker reads interleaved red and dark bytes from the start of guest state.
    uint8_t pixels[2 * AREA];
    Dye field[2][AREA];
    // The brush leaves a displacement in subcell units.
    Velocity wake[AREA];
    Dye pigmentCurve[PIGMENT_MAX + 1];
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
static inline float clamp01(float v) { return clamp(v, 0.0f, 1.0f); }

static float sin_turns(float turns) {
    // Bias the supported inputs before truncating to the nearest whole turn.
    float t = turns - (float)(int)(turns + 32.5f) + 32.0f;
    if (t > .25f) t = .5f - t;
    if (t < -.25f) t = -.5f - t;
    float x = TAU * t;
    float xx = x * x;
    return x * (1.0f + xx * (-0.166655f + xx * (0.0083109f + xx * (-0.0001849f))));
}

// The initial offset and the sampling displacement cross each edge at most once.
static inline int wrap_once(int x, int count) {
    return x < 0 ? x + count : (x >= count ? x - count : x);
}

static void seed(Ink *ink, unsigned originX, unsigned originY) {
    int dx = ((originX & POINTER_MASK) * WIDTH) >> POINTER_BITS;
    int dy = ((originY & POINTER_MASK) * HEIGHT) >> POINTER_BITS;
    ink->initialRed = ink->initialDark = 0;
    for (int y = 0; y < HEIGHT; y++) {
        const int row = wrap_once(y + dy, HEIGHT) * WIDTH;
        const float v = (float)y / HEIGHT;
        for (int x = 0; x < WIDTH; x++) {
            const float u = (float)x / WIDTH;
            const float warpedU = u + .18f * sin_turns(v + .03f) + .05f * sin_turns(v * 2.0f - u);
            const float warpedV = v + .23f * sin_turns(u + .13f) + .06f * sin_turns(u * 3.0f + v);
            const float redPattern = .83f * sin_turns(warpedU + warpedV)
                                   + .61f * sin_turns(2.0f * warpedV - warpedU + .22f)
                                   + .35f * sin_turns(2.0f * warpedU + warpedV - .13f)
                                   + .16f * sin_turns(3.0f * warpedU + 2.0f * warpedV);
            const float darkPattern = .63f * sin_turns(2.0f * warpedU - warpedV + .05f)
                                    + .46f * sin_turns(warpedU + 2.0f * warpedV - .18f)
                                    + .22f * sin_turns(3.0f * warpedV + .30f);
            const float red = .11f + .78f * clamp01(.5f + .73f * redPattern);
            const float dark = .12f + .76f * clamp01(.5f + .82f * darkPattern);
            const unsigned redAmount = (unsigned)(red * PIGMENT_MAX);
            const unsigned darkAmount = (unsigned)(dark * PIGMENT_MAX);
            const int i = row + wrap_once(x + dx, WIDTH);

            ink->field[0][i] = pack_dye(redAmount, darkAmount);
            ink->initialRed += redAmount;
            ink->initialDark += darkAmount;
            ink->wake[i] = (Velocity){0, 0};
            store_pixel(ink, i, redAmount, darkAmount);
        }
    }
    ink->totalRed = ink->initialRed;
    ink->totalDark = ink->initialDark;
}

typedef struct {
    float x, y;
    float motionX, motionY;
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
    float response = 1.10f / (1.0f + .10f * __builtin_sqrtf(speed2));
    // Center the force between the last and current samples to cover quick gestures.
    brush.x = x - clamp(vx, -10.0f, 10.0f) * .44f;
    brush.y = y - clamp(vy, -10.0f, 10.0f) * .44f;
    brush.motionX = vx * response;
    brush.motionY = vy * response;
    brush.left = (int)max(0.0f, brush.x - BRUSH_RADIUS);
    brush.right = (int)min(WIDTH, brush.x + (BRUSH_RADIUS + 1));
    brush.top = (int)max(0.0f, brush.y - BRUSH_RADIUS);
    brush.bottom = (int)min(HEIGHT, brush.y + (BRUSH_RADIUS + 1));
    return brush;
}

static void add_brush_wake(Ink *ink, Brush brush) {
    const float inverseRadiusSquared = 1.0f / (BRUSH_RADIUS * BRUSH_RADIUS);
    for (int y = brush.top; y < brush.bottom; y++) {
        for (int x = brush.left; x < brush.right; x++) {
            const float u = (float)x + .5f - brush.x;
            const float v = (float)y + .5f - brush.y;
            const float falloff = max(0.0f, 1.0f - (u * u + v * v) * inverseRadiusSquared);
            const float edge = min(min((float)x, WIDTH - (float)x - 1.0f),
                                   min((float)y, HEIGHT - (float)y - 1.0f));
            const float amount = falloff * falloff * min(1.0f, edge * .32f);
            // A compact curl gives the stroke a return flow around its path.
            const float twist = (4.0f * inverseRadiusSquared) * (brush.motionX * v - brush.motionY * u);
            const float impulseX = 32.0f * amount * (falloff * brush.motionX - v * twist);
            const float impulseY = 32.0f * amount * (falloff * brush.motionY + u * twist);
            Velocity *wake = &ink->wake[y * WIDTH + x];
            wake->x = (int16_t)clamp(wake->x + impulseX, -WAKE_LIMIT, WAKE_LIMIT);
            wake->y = (int16_t)clamp(wake->y + impulseY, -WAKE_LIMIT, WAKE_LIMIT);
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

static inline Dye sample_dye(const Dye *field, int x, int y) {
    // The signed shift floors sample positions on the x86-64 and Wasm targets.
    const int col = x >> SUBCELL_BITS;
    const int row = y >> SUBCELL_BITS;
    const int left = wrap_once(col, WIDTH);
    const int right = wrap_once(col + 1, WIDTH);
    const int top = wrap_once(row, HEIGHT) * WIDTH;
    const int bottom = wrap_once(row + 1, HEIGHT) * WIDTH;
    const unsigned fx = (unsigned)x & SUBCELL_MASK;
    const unsigned fy = (unsigned)y & SUBCELL_MASK;
    const Dye above = interpolate(field[top + left], field[top + right], fx);
    const Dye below = interpolate(field[bottom + left], field[bottom + right], fx);
    return interpolate(above, below, fy);
}

static void rebuild_pigment_curve(Ink *ink) {
    // Recover pigment lost to interpolation and keep moving interfaces crisp.
    const float gain = .66f / (AREA * (float)PIGMENT_MAX);
    const float redBias = clamp((ink->initialRed - ink->totalRed) * gain, -.019f, .019f);
    const float darkBias = clamp((ink->initialDark - ink->totalDark) * gain, -.019f, .019f);
    for (int i = 0; i <= PIGMENT_MAX; i++) {
        float v = (float)i / PIGMENT_MAX;
        float shape = .048f * (v - .5f);
        float strength = v * (1.0f - v);
        unsigned red = (unsigned)(clamp01(v + strength * (shape + redBias)) * PIGMENT_MAX + .5f);
        unsigned dark = (unsigned)(clamp01(v + strength * (shape + darkBias)) * PIGMENT_MAX + .5f);
        ink->pigmentCurve[i] = pack_dye(red, dark);
    }
}

static void build_currents(uint32_t tick, int row[HEIGHT], int column[WIDTH]) {
    const float time = (float)(tick & (CURRENT_PERIOD - 1)) * (1.0f / CURRENT_PERIOD);
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
}

static void advect(Ink *ink, uint32_t tick) {
    int rowCurrent[HEIGHT], columnCurrent[WIDTH];
    build_currents(tick, rowCurrent, columnCurrent);
    rebuild_pigment_curve(ink);

    // Read the previous field while filling the next one.
    const unsigned next = tick & 1u;
    const Dye *source = ink->field[next ^ 1u];
    Dye *dest = ink->field[next];
    int totalRed = 0, totalDark = 0;
    for (int y = 0; y < HEIGHT; y++) {
        for (int x = 0; x < WIDTH; x++) {
            int i = y * WIDTH + x;
            Velocity wake = ink->wake[i];
            ink->wake[i] = (Velocity){
                (int16_t)((wake.x * 57) / 64),
                (int16_t)((wake.y * 57) / 64),
            };
            const int sampleX = x * SUBCELL_SCALE - rowCurrent[y] - wake.x;
            const int sampleY = y * SUBCELL_SCALE - columnCurrent[x] - wake.y;
            const Dye color = sample_dye(source, sampleX, sampleY);
            const unsigned red = dye_red(ink->pigmentCurve[dye_red(color)]);
            const unsigned dark = dye_dark(ink->pigmentCurve[dye_dark(color)]);
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
// Positions are normalized to 16 bits. The previous position packs x in the low half.
#if defined(__ELF__)
__attribute__((section(".text.step")))
#endif
void step(Ink *ink, uint32_t tick, uint32_t x, uint32_t y, uint32_t previous, uint32_t active) {
    if (tick == 0) {
        // The first position places the dye before the brush starts moving it.
        seed(ink, x, y);
        return;
    }

    add_brush_wake(ink, brush_at(x, y, previous, active));
    advect(ink, tick);
}
