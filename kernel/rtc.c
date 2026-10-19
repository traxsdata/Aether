/*
 * rtc.c - reads wall-clock time from the CMOS RTC. Read once, then advanced
 * from the timer tick, because each CMOS read is slow I/O.
 */
#include "rtc.h"
#include "cpu.h"
#include "kernel.h"

static uint8_t cmos(uint8_t reg) {
    outb(0x70, reg | 0x80); /* bit 7: keep NMIs disabled while we poke CMOS */
    return inb(0x71);
}

static int bcd(int v) { return (v & 0x0F) + (v >> 4) * 10; }

static int64_t base_seconds = -1; /* seconds since midnight 2000-01-01 at boot_tick */
static uint64_t base_tick;
static struct rtc_time base_date;

static int days_in_month(int y, int m) {
    static const int d[] = {31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31};
    return d[m - 1] + (m == 2 && ((y % 4 == 0 && y % 100 != 0) || y % 400 == 0));
}

static void read_cmos(struct rtc_time *t) {
    while (cmos(0x0A) & 0x80) /* update in progress */
        ;
    uint8_t sec = cmos(0x00), min = cmos(0x02), hour = cmos(0x04);
    uint8_t day = cmos(0x07), mon = cmos(0x08), year = cmos(0x09), regb = cmos(0x0B);
    int pm = hour & 0x80;
    hour &= 0x7F;
    if (!(regb & 0x04)) { /* BCD mode */
        sec = (uint8_t)bcd(sec), min = (uint8_t)bcd(min), hour = (uint8_t)bcd(hour);
        day = (uint8_t)bcd(day), mon = (uint8_t)bcd(mon), year = (uint8_t)bcd(year);
    }
    if (!(regb & 0x02)) { /* 12-hour mode */
        hour %= 12;
        if (pm) hour += 12;
    }
    t->year = (uint16_t)(2000 + year);
    t->month = mon, t->day = day, t->hour = hour, t->minute = min, t->second = sec;
}

void rtc_now(struct rtc_time *t) {
    if (base_seconds < 0) {
        read_cmos(&base_date);
        base_seconds = base_date.hour * 3600 + base_date.minute * 60 + base_date.second;
        base_tick = ticks;
    }
    int64_t s = base_seconds + (int64_t)((ticks - base_tick) / TICK_HZ);
    *t = base_date;
    t->hour = (uint8_t)((s / 3600) % 24);
    t->minute = (uint8_t)((s / 60) % 60);
    t->second = (uint8_t)(s % 60);
    for (int64_t days = s / 86400; days > 0; days--) {
        if (++t->day > days_in_month(t->year, t->month)) {
            t->day = 1;
            if (++t->month > 12) t->month = 1, t->year++;
        }
    }
}

/* Seconds since 1970-01-01 (UTC as far as the CMOS clock knows). */
uint64_t rtc_unix(void) {
    struct rtc_time t;
    rtc_now(&t);
    uint64_t days = 0;
    for (int y = 1970; y < t.year; y++) days += 365 + ((y % 4 == 0 && y % 100 != 0) || y % 400 == 0);
    for (int m = 1; m < t.month; m++) days += (uint64_t)days_in_month(t.year, m);
    days += (uint64_t)(t.day - 1);
    return days * 86400 + (uint64_t)t.hour * 3600 + (uint64_t)t.minute * 60 + t.second;
}
