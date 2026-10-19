/*
 * probe.c - a deliberately misbehaving program for the terminal's `sectest`
 * command. Each test (selected by the spawn argument) tries something a
 * malicious app might; the kernel should stop every one of them.
 *
 * Tests that the CPU blocks end with the kernel killing this process (the
 * terminal sees the exception number). Tests that the kernel refuses end
 * with exit code 42 ("refused, as it should be"). It runs as an untrusted
 * app with only a private folder (CAP_STORAGE).
 */
#include "lib/sys.h"

#define REFUSED 42
#define ALLOWED 1 /* the attack worked: a security bug */

static void target(void) {}

int main(uint64_t test) {
    switch (test) {
    case 1: { /* read kernel memory */
        volatile uint64_t *kernel = (volatile uint64_t *)0xFFFFFFFF80100000ull;
        return (int)(*kernel & 0) + ALLOWED;
    }
    case 2: { /* overwrite our own code (code pages are read-only) */
        volatile uint8_t *code = (volatile uint8_t *)(uint64_t)&target;
        *code = 0xC3;
        return ALLOWED;
    }
    case 3: { /* run code placed on the stack (stack is no-execute) */
        volatile uint8_t shellcode[16];
        const uint8_t code[6] = {0xB8, 0x2A, 0, 0, 0, 0xC3}; /* mov eax, 42; ret */
        for (int i = 0; i < 6; i++) shellcode[i] = code[i];
        int (*fn)(void) = (int (*)(void))(uintptr_t)shellcode;
        fn();
        return ALLOWED;
    }
    case 4: /* privileged instruction */
        __asm__ volatile("cli");
        return ALLOWED;
    case 5: { /* talk to hardware directly */
        uint8_t v;
        __asm__ volatile("inb $0x60, %0" : "=a"(v));
        return ALLOWED;
    }
    case 6: { /* use a capability we weren't given */
        struct sys_info si;
        return sys_sysinfo(&si) == -E_PERM ? REFUSED : ALLOWED;
    }
    case 7: { /* trick the kernel into writing over its own memory */
        int64_t r = sys_time((struct time_info *)0xFFFFFFFF80100000ull);
        return r == -E_FAULT ? REFUSED : ALLOWED;
    }
    case 8: { /* start a program to gain more rights */
        return sys_spawn("terminal", ~0u, 0) == -E_PERM ? REFUSED : ALLOWED;
    }
    case 9: /* read the user's files by name (we have no CAP_FILES) */
        return sys_open("/home", O_DIR) == -E_PERM && sys_open("/apps/terminal", O_DIR) == -E_PERM ? REFUSED : ALLOWED;
    case 10: /* climb out of our private folder */
        return sys_open("../terminal", O_DIR) == -E_PERM && sys_open("a/../../x", O_READ) == -E_PERM ? REFUSED : ALLOWED;
    case 11: { /* create memory we could write code into and then run */
        if (sys_mem_map(4096, PROT_READ | PROT_WRITE | PROT_EXEC) != -E_INVAL) return ALLOWED;
        int64_t p = sys_mem_map(4096, PROT_READ | PROT_WRITE);
        if (p < 0) return 3;
        return sys_mem_protect((uint64_t)p, 4096, PROT_READ | PROT_EXEC) == -E_INVAL ? REFUSED : ALLOWED;
    }
    case 12: { /* take more memory than our quota (8 MiB) */
        int64_t p = sys_mem_map(6ull << 20, PROT_READ | PROT_WRITE);
        if (p < 0) return 3;
        return sys_mem_map(4ull << 20, PROT_READ | PROT_WRITE) == -E_NOMEM ? REFUSED : ALLOWED;
    }
    }
    return 2;
}
