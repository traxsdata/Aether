/* rtc.h - the CMOS real-time clock. */
#pragma once
#include <stdint.h>

struct rtc_time {
    uint16_t year;
    uint8_t month, day, hour, minute, second;
};

void rtc_now(struct rtc_time *t);
uint64_t rtc_unix(void);
