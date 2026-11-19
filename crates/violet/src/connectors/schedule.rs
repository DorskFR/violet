//! When a connector runs: `every <n>m|h|d`, `daily HH:MM`, `weekly <mon..sun> HH:MM` or
//! `monthly <1-28> HH:MM`, the clock times in the configured UTC offset.

use std::fmt;

use time::{Date, Duration, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset, Weekday};

const FIRST_BACKOFF: Duration = Duration::seconds(30);
const MAX_BACKOFF: Duration = Duration::hours(6);
const LAST_MONTH_DAY: u8 = 28;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schedule {
	Every(Duration),
	Daily(Time),
	Weekly(Weekday, Time),
	Monthly(u8, Time),
}

fn clock(raw: &str) -> Option<Time> {
	let (hour, minute) = raw.split_once(':')?;
	let two_digits = |part: &str| part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_digit());
	if !two_digits(hour) || !two_digits(minute) {
		return None;
	}
	Time::from_hms(hour.parse().ok()?, minute.parse().ok()?, 0).ok()
}

fn weekday(raw: &str) -> Option<Weekday> {
	Some(match raw {
		"mon" => Weekday::Monday,
		"tue" => Weekday::Tuesday,
		"wed" => Weekday::Wednesday,
		"thu" => Weekday::Thursday,
		"fri" => Weekday::Friday,
		"sat" => Weekday::Saturday,
		"sun" => Weekday::Sunday,
		_ => return None,
	})
}

fn weekday_name(day: Weekday) -> &'static str {
	match day {
		Weekday::Monday => "mon",
		Weekday::Tuesday => "tue",
		Weekday::Wednesday => "wed",
		Weekday::Thursday => "thu",
		Weekday::Friday => "fri",
		Weekday::Saturday => "sat",
		Weekday::Sunday => "sun",
	}
}

fn interval(raw: &str) -> Option<Duration> {
	let unit = raw.chars().next_back()?;
	let digits = &raw[..raw.len() - unit.len_utf8()];
	if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
		return None;
	}
	let count: i64 = digits.parse().ok().filter(|count| *count >= 1)?;
	let seconds = match unit {
		'm' => 60,
		'h' => 3600,
		'd' => 86_400,
		_ => return None,
	};
	count.checked_mul(seconds).map(Duration::seconds)
}

impl Schedule {
	pub fn parse(raw: &str) -> Result<Self, String> {
		let words: Vec<&str> = raw.split_ascii_whitespace().collect();
		let schedule = match words.as_slice() {
			["every", every] => interval(every).map(Self::Every),
			["daily", at] => clock(at).map(Self::Daily),
			["weekly", day, at] => weekday(day)
				.zip(clock(at))
				.map(|(day, at)| Self::Weekly(day, at)),
			["monthly", day, at] if day.bytes().all(|byte| byte.is_ascii_digit()) => day
				.parse()
				.ok()
				.filter(|day| (1..=LAST_MONTH_DAY).contains(day))
				.zip(clock(at))
				.map(|(day, at)| Self::Monthly(day, at)),
			_ => None,
		};
		schedule.ok_or_else(|| {
			format!(
				"unknown schedule \"{raw}\": use every <n>m|h|d, daily HH:MM, \
				 weekly <mon..sun> HH:MM or monthly <1-{LAST_MONTH_DAY}> HH:MM"
			)
		})
	}

	/// The first run strictly after `after`.
	pub fn next_after(&self, after: OffsetDateTime, offset: UtcOffset) -> OffsetDateTime {
		let today = after.to_offset(offset).date();
		let at = |date: Date, time: Time| PrimitiveDateTime::new(date, time).assume_offset(offset);
		match *self {
			Self::Every(every) => after + every,
			Self::Daily(time) => {
				let run = at(today, time);
				if run > after {
					run
				} else {
					run + Duration::days(1)
				}
			}
			Self::Weekly(day, time) => {
				let ahead = i64::from(
					(7 + day.number_days_from_monday() - today.weekday().number_days_from_monday())
						% 7,
				);
				let run = at(today, time) + Duration::days(ahead);
				if run > after {
					run
				} else {
					run + Duration::days(7)
				}
			}
			Self::Monthly(day, time) => {
				let run = at(today.replace_day(day).unwrap_or(today), time);
				if run > after {
					return run;
				}
				let (year, month) = match today.month() {
					time::Month::December => (today.year() + 1, time::Month::January),
					month => (today.year(), month.next()),
				};
				at(
					Date::from_calendar_date(year, month, day).unwrap_or(today),
					time,
				)
			}
		}
	}
}

impl fmt::Display for Schedule {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let clock = |time: Time| format!("{:02}:{:02}", time.hour(), time.minute());
		match *self {
			Self::Every(every) => {
				let seconds = every.whole_seconds();
				match (seconds % 86_400, seconds % 3600) {
					(0, _) => write!(f, "every {}d", seconds / 86_400),
					(_, 0) => write!(f, "every {}h", seconds / 3600),
					_ => write!(f, "every {}m", seconds / 60),
				}
			}
			Self::Daily(time) => write!(f, "daily {}", clock(time)),
			Self::Weekly(day, time) => write!(f, "weekly {} {}", weekday_name(day), clock(time)),
			Self::Monthly(day, time) => write!(f, "monthly {day} {}", clock(time)),
		}
	}
}

/// `+09:00`, `-03:30` or `Z`.
pub fn parse_offset(raw: &str) -> Result<UtcOffset, String> {
	let invalid = || format!("\"{raw}\" is not a UTC offset such as +09:00");
	if raw == "Z" {
		return Ok(UtcOffset::UTC);
	}
	let bytes = raw.as_bytes();
	let shaped = bytes.len() == 6
		&& matches!(bytes[0], b'+' | b'-')
		&& bytes[3] == b':'
		&& [1, 2, 4, 5].iter().all(|at| bytes[*at].is_ascii_digit());
	if !shaped {
		return Err(invalid());
	}
	let sign: i8 = if bytes[0] == b'-' { -1 } else { 1 };
	let hours: i8 = raw[1..3].parse().map_err(|_| invalid())?;
	let minutes: i8 = raw[4..6].parse().map_err(|_| invalid())?;
	if minutes > 59 {
		return Err(invalid());
	}
	UtcOffset::from_hms(sign * hours, sign * minutes, 0).map_err(|_| invalid())
}

/// How long to wait before trying again after `failures` failed runs in a row.
pub fn backoff(failures: u32) -> Duration {
	let doublings = failures.saturating_sub(1).min(16);
	(FIRST_BACKOFF * 2_i32.pow(doublings)).min(MAX_BACKOFF)
}

/// When the next run is due: the retry of a failed run, else the first scheduled time after the
/// last run that succeeded, which is in the past when a run was missed. Now when none did.
pub fn next_run(
	schedule: &Schedule,
	offset: UtcOffset,
	last_success: Option<OffsetDateTime>,
	retry_at: Option<OffsetDateTime>,
	now: OffsetDateTime,
) -> OffsetDateTime {
	match (retry_at, last_success) {
		(Some(retry), _) => retry,
		(None, Some(last)) => schedule.next_after(last, offset),
		(None, None) => now,
	}
}

#[cfg(test)]
mod tests {
	use time::macros::{datetime, offset, time};

	use super::*;

	#[test]
	fn the_grammar_is_parsed_and_written_back() {
		for (raw, schedule) in [
			("every 15m", Schedule::Every(Duration::minutes(15))),
			("every 6h", Schedule::Every(Duration::hours(6))),
			("every 2d", Schedule::Every(Duration::days(2))),
			("daily 07:05", Schedule::Daily(time!(07:05))),
			(
				"weekly sun 23:59",
				Schedule::Weekly(Weekday::Sunday, time!(23:59)),
			),
			("monthly 26 09:00", Schedule::Monthly(26, time!(09:00))),
		] {
			assert_eq!(Schedule::parse(raw), Ok(schedule), "{raw}");
			assert_eq!(schedule.to_string(), raw);
		}
		assert_eq!(
			Schedule::parse("  monthly   1  00:00 "),
			Ok(Schedule::Monthly(1, time!(00:00)))
		);
		for raw in [
			"",
			"hourly",
			"every",
			"every 0m",
			"every -5m",
			"every 5",
			"every 5s",
			"every m",
			"every 1.5h",
			"daily",
			"daily 9:00",
			"daily 24:00",
			"daily 09:60",
			"daily 09:00:00",
			"weekly 09:00",
			"weekly monday 09:00",
			"weekly Mon 09:00",
			"monthly 0 09:00",
			"monthly 29 09:00",
			"monthly +5 09:00",
			"monthly last 09:00",
			"monthly 26",
			"daily 09:00 extra",
		] {
			let error = Schedule::parse(raw).unwrap_err();
			assert!(error.contains("unknown schedule"), "{raw}: {error}");
		}
	}

	#[test]
	fn offsets_are_parsed() {
		assert_eq!(parse_offset("+09:00"), Ok(offset!(+9)));
		assert_eq!(parse_offset("-03:30"), Ok(offset!(-3:30)));
		assert_eq!(parse_offset("+00:00"), Ok(UtcOffset::UTC));
		assert_eq!(parse_offset("Z"), Ok(UtcOffset::UTC));
		for raw in [
			"", "9", "+9", "+9:00", "09:00", "+0900", "+09:60", "+26:00", "JST",
		] {
			assert!(parse_offset(raw).is_err(), "{raw}");
		}
	}

	#[test]
	fn an_interval_counts_from_the_last_run() {
		let schedule = Schedule::Every(Duration::hours(6));
		assert_eq!(
			schedule.next_after(datetime!(2026-11-19 10:30 UTC), offset!(+9)),
			datetime!(2026-11-19 16:30 UTC)
		);
	}

	#[test]
	fn a_daily_run_is_today_or_tomorrow_in_the_offset() {
		let schedule = Schedule::Daily(time!(09:00));
		let tokyo = offset!(+9);
		assert_eq!(
			schedule.next_after(datetime!(2026-11-19 08:59 +9), tokyo),
			datetime!(2026-11-19 09:00 +9)
		);
		assert_eq!(
			schedule.next_after(datetime!(2026-11-19 09:00 +9), tokyo),
			datetime!(2026-11-20 09:00 +9)
		);
		assert_eq!(
			schedule.next_after(datetime!(2026-11-18 23:30 UTC), tokyo),
			datetime!(2026-11-19 00:00 UTC),
			"23:30 UTC is already the 19th in Tokyo"
		);
		assert_eq!(
			schedule.next_after(datetime!(2026-12-31 12:00 +9), tokyo),
			datetime!(2027-01-01 09:00 +9)
		);
	}

	#[test]
	fn a_weekly_run_is_the_next_such_weekday() {
		let schedule = Schedule::Weekly(Weekday::Monday, time!(06:00));
		let paris = offset!(+1);
		assert_eq!(datetime!(2026-11-19 12:00 +1).weekday(), Weekday::Thursday);
		assert_eq!(
			schedule.next_after(datetime!(2026-11-19 12:00 +1), paris),
			datetime!(2026-11-23 06:00 +1)
		);
		assert_eq!(
			schedule.next_after(datetime!(2026-11-23 05:59 +1), paris),
			datetime!(2026-11-23 06:00 +1)
		);
		assert_eq!(
			schedule.next_after(datetime!(2026-11-23 06:00 +1), paris),
			datetime!(2026-11-30 06:00 +1)
		);
		assert_eq!(
			Schedule::Weekly(Weekday::Wednesday, time!(06:00))
				.next_after(datetime!(2026-11-19 12:00 +1), paris),
			datetime!(2026-11-25 06:00 +1)
		);
	}

	#[test]
	fn a_monthly_run_is_this_month_or_the_next() {
		let schedule = Schedule::Monthly(26, time!(09:00));
		let tokyo = offset!(+9);
		assert_eq!(
			schedule.next_after(datetime!(2026-11-19 12:00 +9), tokyo),
			datetime!(2026-11-26 09:00 +9)
		);
		assert_eq!(
			schedule.next_after(datetime!(2026-11-26 09:00 +9), tokyo),
			datetime!(2026-12-26 09:00 +9)
		);
		assert_eq!(
			schedule.next_after(datetime!(2026-12-26 09:00:01 +9), tokyo),
			datetime!(2027-01-26 09:00 +9)
		);
		assert_eq!(
			Schedule::Monthly(28, time!(00:00)).next_after(datetime!(2027-01-31 12:00 +9), tokyo),
			datetime!(2027-02-28 00:00 +9)
		);
	}

	#[test]
	fn the_backoff_doubles_from_thirty_seconds_to_six_hours() {
		assert_eq!(backoff(1), Duration::seconds(30));
		assert_eq!(backoff(2), Duration::seconds(60));
		assert_eq!(backoff(3), Duration::seconds(120));
		assert_eq!(backoff(10), Duration::seconds(15_360));
		assert_eq!(backoff(11), Duration::hours(6));
		assert_eq!(backoff(u32::MAX), Duration::hours(6));
		assert_eq!(backoff(0), Duration::seconds(30));
	}

	#[test]
	fn a_missed_run_is_due_at_once_and_a_kept_one_is_not_rerun() {
		let schedule = Schedule::Monthly(26, time!(09:00));
		let tokyo = offset!(+9);
		let now = datetime!(2026-11-28 15:00 +9);
		assert_eq!(next_run(&schedule, tokyo, None, None, now), now);
		let missed = next_run(
			&schedule,
			tokyo,
			Some(datetime!(2026-10-26 09:00:04 +9)),
			None,
			now,
		);
		assert_eq!(missed, datetime!(2026-11-26 09:00 +9));
		assert!(missed <= now);
		let kept = next_run(
			&schedule,
			tokyo,
			Some(datetime!(2026-11-26 09:00:04 +9)),
			None,
			now,
		);
		assert_eq!(kept, datetime!(2026-12-26 09:00 +9));
		let caught_up = next_run(&schedule, tokyo, Some(now), None, now);
		assert_eq!(caught_up, datetime!(2026-12-26 09:00 +9));
		let retry = now + backoff(2);
		assert_eq!(
			next_run(
				&schedule,
				tokyo,
				Some(datetime!(2026-10-26 09:00 +9)),
				Some(retry),
				now
			),
			retry
		);
	}
}
