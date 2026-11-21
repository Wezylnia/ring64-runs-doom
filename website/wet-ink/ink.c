#include <stdint.h>

#define WIDTH 192
#define HEIGHT 108
#define AREA (WIDTH * HEIGHT)
#define WAKE_SCALE 64
#define TAU 6.28318530718f

typedef uint32_t Dye;
typedef struct { int16_t x, y; } Velocity;
typedef struct {
    uint8_t pixels[2 * AREA];
    Dye dye[AREA];
    Dye transported[AREA];
    Velocity wake[AREA];
    uint32_t tighten[1024];
    int initialRed, initialDark;
    int totalRed, totalDark;
} Ink;

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

static void seed(Ink *ink) {
    ink->initialRed = ink->initialDark = 0;
    for (int y = 0; y < HEIGHT; y++) {
        for (int x = 0; x < WIDTH; x++) {
            float u = (float)x / WIDTH, v = (float)y / HEIGHT;
            float s = u + .18f * sin_turns(v + .03f) + .05f * sin_turns(v * 2.0f - u);
            float t = v + .23f * sin_turns(u + .13f) + .06f * sin_turns(u * 3.0f + v);
            float sheet = .83f * sin_turns(s + t) + .61f * sin_turns(2.0f * t - s + .22f)
                        + .35f * sin_turns(2.0f * s + t - .13f) + .16f * sin_turns(3.0f * s + 2.0f * t);
            float layer = .63f * sin_turns(2.0f * s - t + .05f) + .46f * sin_turns(s + 2.0f * t - .18f)
                        + .22f * sin_turns(3.0f * t + .30f);
            int i = y * WIDTH + x;
            float red = .11f + .78f * clamp(.5f + .73f * sheet, 0.0f, 1.0f);
            float dark = .12f + .76f * clamp(.5f + .82f * layer, 0.0f, 1.0f);
            unsigned r = (unsigned)(red * 1023), d = (unsigned)(dark * 1023);
            ink->dye[i] = r | (d << 16);
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
    float lastX = (float)(previous & 65535) * (WIDTH / 65536.0f);
    float lastY = (float)(previous >> 16) * (HEIGHT / 65536.0f);
    float x = (float)px * (WIDTH / 65536.0f);
    float y = (float)py * (HEIGHT / 65536.0f);
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

static inline int wrap(int x, int count) { return x < 0 ? x + count : (x >= count ? x - count : x); }

static inline Dye interpolate(Dye a, Dye b, unsigned weight) {
    // Two 10-bit channels fit in independent 16-bit lanes during the weighted sum.
    return (((64 - weight) * a + weight * b + 0x00200020) & 0xffc0ffc0) >> 6;
}

static void sharpen(Ink *ink) {
    // Correct slow volume loss at the moving interfaces.
    const float gain = .66f / (AREA * 1023.0f);
    const float redBias = clamp((ink->initialRed - ink->totalRed) * gain, -.019f, .019f);
    const float darkBias = clamp((ink->initialDark - ink->totalDark) * gain, -.019f, .019f);
    for (int i = 0; i <= 1023; i++) {
        float v = (float)i / 1023.0f;
        float shape = .048f * (v - .5f);
        float strength = v * (1.0f - v);
        unsigned red = (unsigned)(clamp(v + strength * (shape + redBias), 0.0f, 1.0f) * 1023.0f + .5f);
        unsigned dark = (unsigned)(clamp(v + strength * (shape + darkBias), 0.0f, 1.0f) * 1023.0f + .5f);
        ink->tighten[i] = red | (dark << 16);
    }
}

static void advect(Ink *ink, uint32_t tick) {
    int row[HEIGHT], column[WIDTH];
    float time = (float)(tick & 32767) * (1.0f / 32768.0f);
    for (int y = 0; y < HEIGHT; y++) {
        float v = (float)y / HEIGHT;
        row[y] = (int)((WAKE_SCALE * .625f) * (.10f + .22f * sin_turns(v - 31.0f * time)
                               + .11f * sin_turns(2.0f * v + 13.0f * time)));
    }
    for (int x = 0; x < WIDTH; x++) {
        float u = (float)x / WIDTH;
        column[x] = (int)((WAKE_SCALE * .625f) * (.05f + .27f * sin_turns(u + 23.0f * time + .37f)
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
            ink->wake[i] = (Velocity){ (int16_t)((wake.x * 57) / 64), (int16_t)((wake.y * 57) / 64) };
            int sx = x * 64 - row[y] - wake.x;
            int sy = y * 64 - column[x] - wake.y;
            int ox = sx >> 6, oy = sy >> 6;
            int l = wrap(ox, WIDTH), r = wrap(ox + 1, WIDTH);
            int n = wrap(oy, HEIGHT) * WIDTH, s = wrap(oy + 1, HEIGHT) * WIDTH;
            unsigned fx = (unsigned)sx & 63, fy = (unsigned)sy & 63;
            Dye a = interpolate(source[n + l], source[n + r], fx);
            Dye b = interpolate(source[s + l], source[s + r], fx);
            Dye color = interpolate(a, b, fy);
            unsigned red = ink->tighten[color & 1023] & 1023;
            unsigned dark = ink->tighten[color >> 16] >> 16;
            totalRed += red;
            totalDark += dark;
            dest[i] = red | dark << 16;
            ink->pixels[2 * i] = (uint8_t)(red >> 2);
            ink->pixels[2 * i + 1] = (uint8_t)(dark >> 2);
        }
    }
    ink->totalRed = totalRed;
    ink->totalDark = totalDark;
}

__attribute__((section(".text.step")))
void step(Ink *ink, uint32_t tick, uint32_t x, uint32_t y, uint32_t previous, uint32_t active) {
    if (tick == 0) {
        seed(ink);
        for (int i = 0; i < AREA; i++) {
            ink->pixels[2 * i] = (uint8_t)((ink->dye[i] & 1023) >> 2);
            ink->pixels[2 * i + 1] = (uint8_t)(ink->dye[i] >> 18);
        }
    } else {
        push(ink, brush_at(x, y, previous, active));
        advect(ink, tick);
    }
}
