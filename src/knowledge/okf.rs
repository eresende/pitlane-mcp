//! OKF (Open Knowledge Format) v0.2 metadata extraction.
//!
//! Extracts the front-matter families defined by the OKF spec (provenance,
//! trust, lifecycle) into a typed [`OkfMeta`] attached to a
//! [`KnowledgeDocument`]. Following the spec's conformance rules, consumers
//! here are deliberately lenient: missing optional fields, unknown `type`
//! values, and unrecognized extra keys never reject a document — unknown keys
//! stay in the document's generic front-matter bag.
//!
//! Spec: <https://github.com/GoogleCloudPlatform/open-knowledge-format> (v0.2).

use serde::{Deserialize, Serialize};

/// Trust tier derived from `verified` (spec §5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub enum TrustTier {
    /// No `verified` key.
    #[default]
    Unverified,
    /// `verified` only by non-`human:` actors.
    MachineConfirmed,
    /// `verified` by at least one `human:<id>` actor.
    HumanReviewed,
}

impl TrustTier {
    /// Lowercase name used in tool responses and filters.
    pub fn as_str(&self) -> &'static str {
        match self {
            TrustTier::Unverified => "unverified",
            TrustTier::MachineConfirmed => "machine-confirmed",
            TrustTier::HumanReviewed => "human-reviewed",
        }
    }

    /// Parse a filter value (case-insensitive; `machine_confirmed` with an
    /// underscore is accepted too).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "unverified" => Some(TrustTier::Unverified),
            "machine-confirmed" => Some(TrustTier::MachineConfirmed),
            "human-reviewed" => Some(TrustTier::HumanReviewed),
            _ => None,
        }
    }

    /// True when `verified` contains at least one `human:` actor.
    fn from_verified(events: &[VerifiedEvent]) -> Self {
        if events
            .iter()
            .any(|e| e.by.as_deref().is_some_and(|by| by.starts_with("human:")))
        {
            TrustTier::HumanReviewed
        } else if events.is_empty() {
            TrustTier::Unverified
        } else {
            TrustTier::MachineConfirmed
        }
    }
}

/// One `verified` entry: `{ by, at }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifiedEvent {
    pub by: Option<String>,
    pub at: Option<String>,
}

/// One `sources` entry (spec §5.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OkfSource {
    pub id: Option<String>,
    pub resource: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub usage_count: Option<u64>,
    pub last_modified: Option<String>,
}

/// Extracted OKF front-matter families.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OkfMeta {
    /// `type` — the only spec-required key. A document with a non-empty
    /// `type` is treated as an OKF concept.
    pub doc_type: String,
    pub description: Option<String>,
    pub resource: Option<String>,
    /// `status`: `draft` | `stable` | `deprecated`; absent ⇒ `stable`.
    pub status: Option<String>,
    /// `stale_after` raw value: the spec §5.5 date-only `YYYY-MM-DD`
    /// spelling, or a full RFC 3339 timestamp.
    pub stale_after: Option<String>,
    pub trust_tier: TrustTier,
    pub generated_by: Option<String>,
    pub generated_at: Option<String>,
    /// Latest `verified[].at`, when present.
    pub verified_at: Option<String>,
    pub sources: Vec<OkfSource>,
    pub usage_window_from: Option<String>,
    pub usage_window_to: Option<String>,
    /// `okf_version` from a bundle-root `index.md` (or any document).
    pub okf_version: Option<String>,
}

impl OkfMeta {
    /// True when this document carries the spec-required `type` key.
    pub fn is_okf(&self) -> bool {
        !self.doc_type.is_empty()
    }
}

/// Extract OKF metadata from parsed front matter. Returns `Ok(None)` when the
/// front matter has no non-empty `type` key (plain Markdown / non-OKF docs).
///
/// Lenient per spec §11: wrong-shaped values for individual fields are
/// skipped, not errors.
pub fn extract_okf(front_matter: &serde_yaml::Value) -> Option<OkfMeta> {
    let map = front_matter.as_mapping()?;
    let doc_type = str_field(map, "type").unwrap_or_default();
    if doc_type.is_empty() {
        return None;
    }

    let verified = verified_events(map);
    let sources = sources_list(map);
    let (usage_window_from, usage_window_to) = usage_window(map);

    let verified_at = verified
        .iter()
        .filter_map(|e| e.at.clone())
        .max()
        .filter(|s| !s.is_empty());

    Some(OkfMeta {
        doc_type,
        description: str_field(map, "description"),
        resource: str_field(map, "resource"),
        status: str_field(map, "status"),
        stale_after: str_field(map, "stale_after"),
        trust_tier: TrustTier::from_verified(&verified),
        generated_by: map_field(map, "generated").and_then(|g| str_field(g, "by")),
        generated_at: map_field(map, "generated").and_then(|g| str_field(g, "at")),
        verified_at,
        sources,
        usage_window_from,
        usage_window_to,
        okf_version: str_field(map, "okf_version"),
    })
}

/// True when the document's `stale_after` (spec §5.5) has passed: a full RFC
/// 3339 instant, or the spec's bare `YYYY-MM-DD` date. Date-only values are
/// compared from UTC midnight of that day, so a concept is stale on/after its
/// `stale_after` day itself — matching the reference implementation's
/// `today >= stale_after`. Malformed values are never stale.
pub fn is_stale(meta: &OkfMeta, now_secs: i64) -> bool {
    meta.stale_after
        .as_deref()
        .and_then(parse_iso8601_secs)
        .is_some_and(|t| now_secs >= t)
}

/// Parse a `stale_after` value into Unix seconds: a full RFC 3339 datetime
/// with explicit offset, or the OKF spec §5.5 date-only `YYYY-MM-DD` spelling
/// (as UTC midnight, so a concept is stale from the start of that day).
/// Returns `None` for anything else, including the basic (`20260101`) and
/// week (`2026-W01-1`) forms the okf reference's strict ISO_DATE grammar
/// rejects.
pub fn parse_iso8601_secs(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp());
    }
    // Strict `YYYY-MM-DD`: exactly four digits, dash, two digits, dash, two
    // digits. `from_ymd_opt` then validates calendar legality (month and day
    // ranges, leap years).
    let bytes = s.as_bytes();
    if bytes.len() != 10
        || !bytes[..4].iter().all(|b| b.is_ascii_digit())
        || bytes[4] != b'-'
        || !bytes[5..7].iter().all(|b| b.is_ascii_digit())
        || bytes[7] != b'-'
        || !bytes[8..].iter().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let year = std::str::from_utf8(&bytes[..4]).ok()?.parse::<i32>().ok()?;
    let month = std::str::from_utf8(&bytes[5..7])
        .ok()?
        .parse::<u32>()
        .ok()?;
    let day = std::str::from_utf8(&bytes[8..]).ok()?.parse::<u32>().ok()?;
    chrono::NaiveDate::from_ymd_opt(year, month, day)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc().timestamp())
}

/// Current time as Unix seconds (0 on clock failure, matching FileState).
pub fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

fn as_str(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn str_field(map: &serde_yaml::Mapping, key: &str) -> Option<String> {
    map.get(serde_yaml::Value::String(key.into()))
        .and_then(as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn map_field<'a>(map: &'a serde_yaml::Mapping, key: &str) -> Option<&'a serde_yaml::Mapping> {
    match map.get(serde_yaml::Value::String(key.into())) {
        Some(serde_yaml::Value::Mapping(m)) => Some(m),
        _ => None,
    }
}

/// `verified`: a list of `{ by, at }` events, or a bare mapping treated as a
/// one-element list (spec §5.2 MUST).
fn verified_events(map: &serde_yaml::Mapping) -> Vec<VerifiedEvent> {
    let key = serde_yaml::Value::String("verified".into());
    let events: Vec<VerifiedEvent> = match map.get(&key) {
        Some(serde_yaml::Value::Sequence(items)) => items
            .iter()
            .filter_map(|v| serde_yaml::from_value(v.clone()).ok())
            .collect(),
        Some(v @ serde_yaml::Value::Mapping(_)) => {
            serde_yaml::from_value(v.clone()).ok().into_iter().collect()
        }
        _ => Vec::new(),
    };
    events
        .into_iter()
        .filter(|e| e.by.is_some() || e.at.is_some())
        .collect()
}

fn sources_list(map: &serde_yaml::Mapping) -> Vec<OkfSource> {
    let Some(serde_yaml::Value::Sequence(items)) =
        map.get(serde_yaml::Value::String("sources".into()))
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|v| {
            let m = v.as_mapping()?;
            // `resource` is required within an entry (spec §5.1).
            str_field(m, "resource")?;
            Some(OkfSource {
                id: str_field(m, "id"),
                resource: str_field(m, "resource"),
                title: str_field(m, "title"),
                author: str_field(m, "author"),
                usage_count: m
                    .get(serde_yaml::Value::String("usage_count".into()))
                    .and_then(|v| v.as_u64()),
                last_modified: str_field(m, "last_modified"),
            })
        })
        .collect()
}

fn usage_window(map: &serde_yaml::Mapping) -> (Option<String>, Option<String>) {
    let Some(m) = map_field(map, "usage_window") else {
        return (None, None);
    };
    (str_field(m, "from"), str_field(m, "to"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn front_matter(yaml: &str) -> serde_yaml::Value {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn minimal_concept_is_okf() {
        let meta = extract_okf(&front_matter("type: Playbook\n")).unwrap();
        assert!(meta.is_okf());
        assert_eq!(meta.doc_type, "Playbook");
        assert_eq!(meta.trust_tier, TrustTier::Unverified);
        assert_eq!(meta.status, None);
    }

    #[test]
    fn missing_or_empty_type_is_not_okf() {
        assert!(extract_okf(&front_matter("title: Just a doc\n")).is_none());
        assert!(extract_okf(&front_matter("type: \"\"\n")).is_none());
        // Scalar front matter (not a mapping) is not OKF.
        assert!(extract_okf(&front_matter("just text")).is_none());
    }

    #[test]
    fn trust_tiers_follow_verified_actors() {
        let unverified = extract_okf(&front_matter("type: Metric\n")).unwrap();
        assert_eq!(unverified.trust_tier, TrustTier::Unverified);

        let machine = extract_okf(&front_matter(
            "type: Metric\nverified: { by: process:nightly, at: 2026-06-26T02:00:00Z }\n",
        ))
        .unwrap();
        assert_eq!(machine.trust_tier, TrustTier::MachineConfirmed);
        assert_eq!(machine.verified_at.as_deref(), Some("2026-06-26T02:00:00Z"));

        // Bare mapping verified counts as one event; latest `at` wins.
        let human = extract_okf(&front_matter(
            "type: Metric\nverified:\n  - { by: human:ana, at: 2026-06-25T09:00:00Z }\n  - { by: process:nightly, at: 2026-06-26T02:00:00Z }\n",
        ))
        .unwrap();
        assert_eq!(human.trust_tier, TrustTier::HumanReviewed);
        assert_eq!(human.verified_at.as_deref(), Some("2026-06-26T02:00:00Z"));
    }

    #[test]
    fn full_front_matter_extracts_every_family() {
        let meta = extract_okf(&front_matter(
            "type: Attested Computation\ntitle: Revenue\ndescription: Recognized revenue.\nresource: /tables/orders\nstatus: deprecated\nstale_after: 2026-09-23T00:00:00Z\nruntime: bigquery\ngenerated: { by: agent/gemini, at: 2026-06-20T22:53:05Z }\nverified: { by: human:ana, at: 2026-06-25T09:00:00Z }\nokf_version: \"0.2\"\nusage_window: { from: 2026-06-01T00:00:00Z, to: 2026-06-30T00:00:00Z }\nsources:\n  - id: rev-policy\n    resource: https://wiki.acme/finance/revenue\n    title: Revenue policy\n    author: team:finance\n    usage_count: 5000\n    last_modified: 2026-04-02T00:00:00Z\n  - title: no resource key — skipped\n",
        ))
        .unwrap();
        assert_eq!(meta.doc_type, "Attested Computation");
        assert_eq!(meta.description.as_deref(), Some("Recognized revenue."));
        assert_eq!(meta.resource.as_deref(), Some("/tables/orders"));
        assert_eq!(meta.status.as_deref(), Some("deprecated"));
        assert_eq!(meta.stale_after.as_deref(), Some("2026-09-23T00:00:00Z"));
        assert_eq!(meta.trust_tier, TrustTier::HumanReviewed);
        assert_eq!(meta.generated_by.as_deref(), Some("agent/gemini"));
        assert_eq!(meta.okf_version.as_deref(), Some("0.2"));
        assert_eq!(meta.sources.len(), 1);
        assert_eq!(meta.sources[0].usage_count, Some(5000));
        assert_eq!(
            meta.usage_window_from.as_deref(),
            Some("2026-06-01T00:00:00Z")
        );
        // Spec-computation fields like `runtime` are not modeled; they stay in
        // the generic bag at the document level.
    }

    #[test]
    fn wrong_shaped_fields_are_skipped_not_errors() {
        let meta = extract_okf(&front_matter(
            "type: Metric\ndescription:\n  - not a string\nverified: nonsense\nsources: not-a-list\n",
        ))
        .unwrap();
        assert_eq!(meta.doc_type, "Metric");
        assert_eq!(meta.description, None);
        assert_eq!(meta.trust_tier, TrustTier::Unverified);
        assert!(meta.sources.is_empty());
    }

    #[test]
    fn staleness_is_absolute_and_lenient() {
        let mut meta = OkfMeta {
            doc_type: "Metric".into(),
            stale_after: Some("2026-09-23T00:00:00Z".into()),
            ..Default::default()
        };
        assert!(is_stale(
            &meta,
            parse_iso8601_secs("2026-09-23T00:00:00Z").unwrap()
        ));
        assert!(!is_stale(
            &meta,
            parse_iso8601_secs("2026-09-22T23:59:59Z").unwrap()
        ));

        meta.stale_after = Some("not a timestamp".into());
        assert!(!is_stale(&meta, i64::MAX / 2));

        meta.stale_after = None;
        assert!(!is_stale(&meta, i64::MAX / 2));
    }

    #[test]
    fn stale_after_date_only_spelling_is_stale_from_that_day() {
        // Spec §5.5 names the bare date; a concept is stale on/after the day.
        let far_past = OkfMeta {
            doc_type: "Metric".into(),
            stale_after: Some("2000-01-01".into()),
            ..Default::default()
        };
        assert!(is_stale(
            &far_past,
            parse_iso8601_secs("2026-09-23T12:00:00Z").unwrap()
        ));

        // Boundary: on the cut-off day itself (UTC midnight) the doc is stale.
        let today = OkfMeta {
            doc_type: "Metric".into(),
            stale_after: Some("2026-09-23".into()),
            ..Default::default()
        };
        assert!(is_stale(
            &today,
            parse_iso8601_secs("2026-09-23T00:00:00Z").unwrap()
        ));

        // Date-only and its RFC 3339 midnight equate to the same instant.
        assert_eq!(
            parse_iso8601_secs("2000-01-01"),
            parse_iso8601_secs("2000-01-01T00:00:00Z")
        );

        // A future cut-off is not stale yet.
        let future = OkfMeta {
            doc_type: "Metric".into(),
            stale_after: Some("2099-12-31".into()),
            ..Default::default()
        };
        assert!(!is_stale(
            &future,
            parse_iso8601_secs("2026-09-23T12:00:00Z").unwrap()
        ));
    }

    #[test]
    fn stale_after_rejects_non_date_spellings() {
        // Basic and week forms (rejected by the reference's ISO_DATE too),
        // non-padded parts, impossible dates, and garbage.
        for bad in [
            "20260101",
            "2026-W01-1",
            "2026-1-1",
            "2026-09-1",
            "2026-13-01",
            "2026-00-01",
            "2026-09-32",
            "2026-02-30",
            "2026-09-23 00:00:00",
            "not a date",
            "",
        ] {
            assert_eq!(parse_iso8601_secs(bad), None, "expected {bad:?} rejected");
        }
        // Full RFC 3339 timestamps still parse through the timestamp path.
        assert!(parse_iso8601_secs("2026-09-23T00:00:00Z").is_some());
        assert!(parse_iso8601_secs("2026-09-23T12:30:00-05:00").is_some());
    }

    #[test]
    fn trust_tier_parse_accepts_filter_spellings() {
        assert_eq!(
            TrustTier::parse("Human-Reviewed"),
            Some(TrustTier::HumanReviewed)
        );
        assert_eq!(
            TrustTier::parse("machine_confirmed"),
            Some(TrustTier::MachineConfirmed)
        );
        assert_eq!(TrustTier::parse("bogus"), None);
    }
}
