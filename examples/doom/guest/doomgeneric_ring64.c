// doomgeneric platform layer for ring64.
#include <stdio.h>
#include <stdint.h>

#include "doomgeneric.h"
#include "i_video.h"
#include "ring64.h"

void DG_Init(void) {}

void DG_DrawFrame(void)
{
    // CMAP256 at 320x200: DG_ScreenBuffer holds palette indices and `colors`
    // the current palette. The host converts both to RGBA.
    ring64_hypercall(R64_HC_FRAME, (long)DG_ScreenBuffer, (long)colors, 0);
}

void DG_SleepMs(uint32_t ms) { ring64_hypercall(R64_HC_SLEEP, ms, 0, 0); }

uint32_t DG_GetTicksMs(void) { return r64_in32(R64_PORT_TICKS); }

int DG_GetKey(int *pressed, unsigned char *key)
{
    uint16_t event = r64_in16(R64_PORT_KEY);
    if (!(event & 0x8000))
        return 0;
    *pressed = (event >> 8) & 1;
    *key = event & 0xff;
    return 1;
}

void DG_SetWindowTitle(const char *title) { ring64_hypercall(R64_HC_TITLE, (long)title, 0, 0); }

int main(int argc, char **argv)
{
    doomgeneric_Create(argc, argv);
    for (;;)
        doomgeneric_Tick();
}
