/* serial.h - COM1 output, for logging in QEMU (-serial) and VMware (serial port to file). */
#pragma once

void serial_init(void);
void serial_putc(char c);
