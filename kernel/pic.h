/* pic.h - legacy 8259 interrupt controllers and the 8254 timer. */
#pragma once

void pic_init(void);          /* remap IRQs to vectors 32-47, all masked */
void pic_unmask(int irq);
void pit_init(unsigned hz);
void irq_handle(int irq);     /* called from isr_dispatch with IF=0 */
void lapic_quiesce(void);     /* mask timers firmware may have left running */
