//! A small iCalendar (RFC 5545) reader: just enough to list one day's events —
//! VEVENTs with DTSTART/DTEND/DURATION, TZID, all-day dates, RRULE/RDATE/EXDATE,
//! RECURRENCE-ID overrides and STATUS:CANCELLED.

use chrono::{DateTime, Duration, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub struct Prop {
    pub name: String,
    pub params: Vec<(String, String)>,
    pub value: String,
}

impl Prop {
    pub fn param(&self, key: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

/// One event occurrence on the requested day, in local time.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub title: String,
    pub location: String,
    pub calendar: String,
    pub color: String,
    pub all_day: bool,
}

fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix(' ').or_else(|| line.strip_prefix('\t')) {
            if let Some(last) = lines.last_mut() {
                last.push_str(rest);
                continue;
            }
        }
        lines.push(line.to_owned());
    }
    lines
}

fn parse_line(line: &str) -> Option<Prop> {
    // The value starts at the first ':' outside double quotes.
    let mut quoted = false;
    let colon = line.char_indices().find(|&(_, c)| {
        if c == '"' {
            quoted = !quoted;
        }
        c == ':' && !quoted
    })?;
    let (head, value) = (&line[..colon.0], &line[colon.0 + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.trim().to_ascii_uppercase();
    let params = parts
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.to_ascii_uppercase(), v.trim_matches('"').to_owned()))
        .collect();
    Some(Prop { name, params, value: value.to_owned() })
}

/// The properties of every VEVENT (nested components like VALARM are skipped).
pub fn vevents(text: &str) -> Vec<Vec<Prop>> {
    let mut out = Vec::new();
    let mut current: Option<Vec<Prop>> = None;
    let mut nested = 0;
    for line in unfold(text) {
        let Some(p) = parse_line(&line) else { continue };
        match (p.name.as_str(), p.value.trim().to_ascii_uppercase().as_str()) {
            ("BEGIN", "VEVENT") if current.is_none() => current = Some(Vec::new()),
            ("BEGIN", _) if current.is_some() => nested += 1,
            ("END", "VEVENT") if nested == 0 => out.extend(current.take()),
            ("END", _) if nested > 0 => nested -= 1,
            _ if nested == 0 => {
                if let Some(props) = current.as_mut() {
                    props.push(p);
                }
            }
            _ => {}
        }
    }
    out
}

pub fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(c) => out.push(c),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A DATE or DATE-TIME value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum When {
    Date(NaiveDate),
    Floating(NaiveDateTime),
    Utc(NaiveDateTime),
    Zoned(NaiveDateTime, chrono_tz::Tz),
}

pub fn parse_when(value: &str, tzid: Option<&str>) -> Option<When> {
    let v = value.trim();
    if v.len() == 8 {
        return NaiveDate::parse_from_str(v, "%Y%m%d").ok().map(When::Date);
    }
    let (v, utc) = match v.strip_suffix('Z') {
        Some(v) => (v, true),
        None => (v, false),
    };
    let dt = NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()?;
    Some(if utc {
        When::Utc(dt)
    } else {
        match tzid.and_then(|t| t.parse::<chrono_tz::Tz>().ok()) {
            Some(tz) => When::Zoned(dt, tz),
            // Unknown TZIDs (e.g. Windows zone names) fall back to local time.
            None => When::Floating(dt),
        }
    })
}

fn local_dt(n: NaiveDateTime) -> DateTime<rrule::Tz> {
    rrule::Tz::LOCAL
        .from_local_datetime(&n)
        .earliest()
        // A time in a DST gap: use the time an hour later.
        .unwrap_or_else(|| rrule::Tz::LOCAL.from_local_datetime(&(n + Duration::hours(1))).earliest().unwrap())
}

impl When {
    pub fn to_dt(self) -> DateTime<rrule::Tz> {
        match self {
            When::Date(d) => local_dt(d.and_hms_opt(0, 0, 0).unwrap()),
            When::Floating(n) => local_dt(n),
            When::Utc(n) => rrule::Tz::UTC.from_utc_datetime(&n),
            When::Zoned(n, tz) => {
                let tz = rrule::Tz::from(tz);
                tz.from_local_datetime(&n).earliest().unwrap_or_else(|| tz.from_utc_datetime(&n))
            }
        }
    }
}

/// `P1D`, `PT1H30M`, `P2W`, `-PT15M`, …
pub fn parse_duration(s: &str) -> Option<Duration> {
    let (sign, s) = match s.trim().strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, s.trim().strip_prefix('+').unwrap_or(s.trim())),
    };
    let s = s.strip_prefix('P')?;
    let (mut total, mut num, mut in_time) = (Duration::zero(), String::new(), false);
    for c in s.chars() {
        match c {
            '0'..='9' => num.push(c),
            'T' => in_time = true,
            unit => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += match (unit, in_time) {
                    ('W', false) => Duration::weeks(n),
                    ('D', false) => Duration::days(n),
                    ('H', true) => Duration::hours(n),
                    ('M', true) => Duration::minutes(n),
                    ('S', true) => Duration::seconds(n),
                    _ => return None,
                };
            }
        }
    }
    Some(total * sign)
}

fn whens(props: &[Prop], name: &str) -> Vec<When> {
    props
        .iter()
        .filter(|p| p.name == name)
        .flat_map(|p| p.value.split(',').filter_map(|v| parse_when(v, p.param("TZID"))).collect::<Vec<_>>())
        .collect()
}

fn text(props: &[Prop], name: &str) -> String {
    props.iter().find(|p| p.name == name).map(|p| unescape(&p.value)).unwrap_or_default()
}

/// Today's (= `day`'s) event occurrences of one feed.
pub fn expand(ics: &str, day: NaiveDate, calendar: &str, color: &str) -> Vec<Event> {
    let events = vevents(ics);
    let day_start = day.and_hms_opt(0, 0, 0).unwrap();
    let day_end = day_start + Duration::days(1);

    // Instances replaced (or cancelled) by RECURRENCE-ID overrides, per UID.
    let mut overridden: HashMap<String, HashSet<DateTime<Utc>>> = HashMap::new();
    for props in &events {
        if let Some(rid) = whens(props, "RECURRENCE-ID").first() {
            overridden.entry(text(props, "UID")).or_default().insert(rid.to_dt().with_timezone(&Utc));
        }
    }

    let mut out = Vec::new();
    for props in &events {
        if text(props, "STATUS").eq_ignore_ascii_case("CANCELLED") {
            continue;
        }
        let Some(&start) = whens(props, "DTSTART").first() else { continue };
        let all_day = matches!(start, When::Date(_));
        let dtstart = start.to_dt();
        let duration = match whens(props, "DTEND").first() {
            Some(end) => end.to_dt() - dtstart,
            None => match props.iter().find(|p| p.name == "DURATION").and_then(|p| parse_duration(&p.value)) {
                Some(d) => d,
                None if all_day => Duration::days(1),
                None => Duration::zero(),
            },
        };
        let is_override = !whens(props, "RECURRENCE-ID").is_empty();
        let rrule = props.iter().find(|p| p.name == "RRULE").map(|p| p.value.clone());
        let starts: Vec<DateTime<rrule::Tz>> = match rrule {
            Some(rule) if !is_override => {
                match occurrences(&rule, dtstart, props, duration, day_start, day_end) {
                    Ok(dates) => {
                        let skip = overridden.get(&text(props, "UID"));
                        dates.into_iter().filter(|d| !skip.is_some_and(|s| s.contains(&d.with_timezone(&Utc)))).collect()
                    }
                    Err(e) => {
                        log::warn!("{calendar}: event {:?}: bad RRULE {rule:?} ({e}); showing only its first occurrence", text(props, "SUMMARY"));
                        vec![dtstart]
                    }
                }
            }
            _ => vec![dtstart],
        };
        for s in starts {
            let (start, end) = if all_day {
                let d = s.with_timezone(&Local).date_naive();
                let days = duration.num_days().max(1);
                (d.and_hms_opt(0, 0, 0).unwrap(), (d + Duration::days(days)).and_hms_opt(0, 0, 0).unwrap())
            } else {
                let start = s.with_timezone(&Local).naive_local();
                (start, start + duration)
            };
            let overlaps = start < day_end && (end > day_start || (end == start && start >= day_start));
            if overlaps {
                out.push(Event {
                    start,
                    end,
                    title: text(props, "SUMMARY"),
                    location: text(props, "LOCATION"),
                    calendar: calendar.to_owned(),
                    color: color.to_owned(),
                    all_day,
                });
            }
        }
    }
    out.sort_by_key(|e| (e.start, e.end));
    out
}

fn occurrences(
    rule: &str,
    dtstart: DateTime<rrule::Tz>,
    props: &[Prop],
    duration: Duration,
    day_start: NaiveDateTime,
    day_end: NaiveDateTime,
) -> Result<Vec<DateTime<rrule::Tz>>, String> {
    let rrule: rrule::RRule<rrule::Unvalidated> = rule.parse().map_err(|e: rrule::RRuleError| e.to_string())?;
    let mut set = rrule.build(dtstart).map_err(|e| e.to_string())?;
    for ex in whens(props, "EXDATE") {
        set = set.exdate(ex.to_dt());
    }
    for r in whens(props, "RDATE") {
        set = set.rdate(r.to_dt());
    }
    // Occurrences that start before the day but are still running count too.
    let from = local_dt(day_start - duration.max(Duration::zero()) - Duration::minutes(1));
    Ok(set.after(from).before(local_dt(day_end)).all(500).dates)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }
    fn t(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn cal(body: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{body}END:VCALENDAR\r\n")
    }

    #[test]
    fn unfolds_and_unescapes() {
        let ics = cal("BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20260105T090000\r\nDTEND:20260105T093000\r\nSUMMARY:Long\r\n  title\\, part two\r\nBEGIN:VALARM\r\nSUMMARY:alarm\r\nEND:VALARM\r\nEND:VEVENT\r\n");
        let ev = expand(&ics, d("2026-01-05"), "work", "#fff");
        assert_eq!(ev.len(), 1);
        assert_eq!((ev[0].title.as_str(), ev[0].start, ev[0].end), ("Long title, part two", t("2026-01-05 09:00"), t("2026-01-05 09:30")));
    }

    #[test]
    fn weekly_recurrence_with_exdate_and_override() {
        let ics = cal(concat!(
            "BEGIN:VEVENT\r\nUID:standup\r\nDTSTART:20260105T090000\r\nDURATION:PT15M\r\nRRULE:FREQ=WEEKLY;BYDAY=MO\r\n",
            "EXDATE:20260112T090000\r\nSUMMARY:Standup\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:standup\r\nRECURRENCE-ID:20260119T090000\r\nDTSTART:20260119T100000\r\nDTEND:20260119T101500\r\nSUMMARY:Standup (moved)\r\nEND:VEVENT\r\n",
        ));
        assert_eq!(expand(&ics, d("2026-01-05"), "", "")[0].start, t("2026-01-05 09:00"));
        assert!(expand(&ics, d("2026-01-12"), "", "").is_empty(), "EXDATE removes the instance");
        let moved = expand(&ics, d("2026-01-19"), "", "");
        assert_eq!(moved.len(), 1, "{moved:?}");
        assert_eq!((moved[0].title.as_str(), moved[0].start), ("Standup (moved)", t("2026-01-19 10:00")));
        assert_eq!(expand(&ics, d("2026-03-02"), "", "").len(), 1);
        assert!(expand(&ics, d("2026-03-03"), "", "").is_empty(), "not on tuesdays");
    }

    #[test]
    fn all_day_cancelled_and_midnight_crossing_events() {
        let ics = cal(concat!(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20260105\r\nDTEND;VALUE=DATE:20260107\r\nSUMMARY:Trip\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:b\r\nDTSTART:20260105T120000\r\nDTEND:20260105T130000\r\nSTATUS:CANCELLED\r\nSUMMARY:Gone\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:c\r\nDTSTART:20260105T230000\r\nDTEND:20260106T010000\r\nSUMMARY:Late\r\nEND:VEVENT\r\n",
        ));
        let titles = |day| expand(&ics, d(day), "", "").into_iter().map(|e| (e.title, e.all_day)).collect::<Vec<_>>();
        assert_eq!(titles("2026-01-05"), vec![("Trip".to_string(), true), ("Late".to_string(), false)]);
        assert_eq!(titles("2026-01-06"), vec![("Trip".to_string(), true), ("Late".to_string(), false)]);
        assert!(titles("2026-01-07").is_empty());
    }

    #[test]
    fn utc_and_zoned_times_convert_to_local() {
        let ics = cal("BEGIN:VEVENT\r\nUID:z\r\nDTSTART;TZID=Asia/Tokyo:20260105T180000\r\nDTEND;TZID=Asia/Tokyo:20260105T190000\r\nSUMMARY:Tokyo\r\nEND:VEVENT\r\n");
        let expected = chrono_tz::Asia::Tokyo.with_ymd_and_hms(2026, 1, 5, 18, 0, 0).unwrap().with_timezone(&Local).naive_local();
        let ev = expand(&ics, expected.date(), "", "");
        assert_eq!(ev[0].start, expected);
        assert_eq!(parse_when("20260105T090000Z", None), Some(When::Utc(t("2026-01-05 09:00"))));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("PT1H30M"), Some(Duration::minutes(90)));
        assert_eq!(parse_duration("P1W2D"), Some(Duration::days(9)));
        assert_eq!(parse_duration("-PT5M"), Some(Duration::minutes(-5)));
        assert_eq!(parse_duration("1H"), None);
    }
}
