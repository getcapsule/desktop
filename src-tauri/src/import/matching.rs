use super::model::{Metadata, PlaybackCandidate};

/// Comparison keys only. Original provider metadata is never rewritten.
pub fn normalize(value: &str) -> String {
    let composed = icu_normalizer::ComposingNormalizer::new_nfkc().normalize(value);
    let cleaned: String = composed
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    cleaned
        .split_whitespace()
        .map(|word| match word {
            "ft" | "featuring" => "feat",
            _ => word,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn normalize_isrc(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_uppercase)
        .collect()
}

fn version_markers(metadata: &Metadata) -> std::collections::BTreeSet<String> {
    let title = normalize(&metadata.title);
    // Album names (for example Dark Lane Demo Tapes) describe releases, not
    // necessarily this track's edition. Track labels remain authoritative.
    let words: std::collections::HashSet<_> = title.split_whitespace().collect();
    let mut markers = std::collections::BTreeSet::new();
    for (marker, forms) in [
        ("live", &["live"][..]),
        ("remix", &["remix", "remixed"][..]),
        ("acoustic", &["acoustic", "unplugged"][..]),
        ("instrumental", &["instrumental", "karaoke"][..]),
        ("remaster", &["remaster", "remastered"][..]),
        ("clean", &["clean", "edited"][..]),
        ("explicit", &["explicit"][..]),
        ("cover", &["cover", "tribute"][..]),
        ("demo", &["demo"][..]),
        ("sped", &["sped", "nightcore"][..]),
        ("slowed", &["slowed"][..]),
    ] {
        if forms.iter().any(|f| words.contains(f)) {
            markers.insert(marker.into());
        }
    }
    if markers.contains("remaster") {
        for word in &words {
            if word.len() == 4 && word.bytes().all(|b| b.is_ascii_digit()) {
                markers.insert(format!("remaster-year-{word}"));
            }
        }
    }
    markers
}

pub fn versions_compatible(a: &Metadata, b: &Metadata) -> bool {
    if matches!((a.explicit,b.explicit),(Some(x),Some(y)) if x!=y) {
        return false;
    }
    version_markers(a) == version_markers(b)
}

fn similarity(a: &str, b: &str) -> f64 {
    let a = normalize(a);
    let b = normalize(b);
    if a.is_empty() || b.is_empty() {
        return 0.;
    }
    if a == b {
        return 1.;
    }
    let left: std::collections::HashSet<_> = a.split_whitespace().collect();
    let right: std::collections::HashSet<_> = b.split_whitespace().collect();
    left.intersection(&right).count() as f64 / left.union(&right).count().max(1) as f64
}

/// Providers may put featured performers in the title, artist list, or both.
/// Preserve whole performer names and require the same primary artist; matching
/// only the primary artist would silently accept missing or different guests.
fn recording_identity(metadata: &Metadata) -> (String, std::collections::BTreeSet<String>) {
    let mut credits: std::collections::BTreeSet<_> = metadata
        .artists
        .iter()
        .map(|artist| normalize(artist))
        .filter(|artist| !artist.is_empty())
        .collect();
    let title = icu_normalizer::ComposingNormalizer::new_nfkc()
        .normalize(&metadata.title)
        .to_lowercase();
    for (index, _) in title.char_indices() {
        if index == 0
            || title[..index]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
        {
            continue;
        }
        for marker in ["featuring ", "feat. ", "feat ", "ft. ", "ft "] {
            let Some(rest) = title[index..].strip_prefix(marker) else {
                continue;
            };
            let (guests, suffix) = rest
                .find([')', ']'])
                .map(|end| (&rest[..end], &rest[end + 1..]))
                .unwrap_or((rest, ""));
            let base = normalize(&format!("{} {}", &title[..index], suffix));
            if base.is_empty() || normalize(guests).is_empty() {
                continue;
            }
            for guest in guests.replace(" and ", ",").split([',', '&', ';']) {
                let guest = normalize(guest);
                if !guest.is_empty() {
                    credits.insert(guest);
                }
            }
            return (base, credits);
        }
    }
    (normalize(&metadata.title), credits)
}

fn credits_equivalent(a: &Metadata, b: &Metadata) -> bool {
    matches!((a.artists.first(), b.artists.first()), (Some(x), Some(y))
        if !normalize(x).is_empty() && normalize(x) == normalize(y))
        && recording_identity(a).1 == recording_identity(b).1
}

pub fn same_recording(a: &Metadata, b: &Metadata) -> bool {
    versions_compatible(a, b)
        && a.explicit == b.explicit
        && recording_identity(a).0 == recording_identity(b).0
        && credits_equivalent(a, b)
        && !matches!((&a.isrc, &b.isrc), (Some(x), Some(y)) if normalize_isrc(x) != normalize_isrc(y))
        && match (a.duration_ms, b.duration_ms) {
            (Some(x), Some(y)) => x > 0 && y > 0 && x.abs_diff(y) <= 2000,
            _ => false,
        }
}

/// Attribute weights are heuristics, not calibrated probabilities. Missing
/// attributes are excluded from the denominator, not evidence of agreement.
pub fn score(a: &Metadata, b: &Metadata) -> (f64, bool) {
    let same_credits = credits_equivalent(a, b);
    let title = if same_credits {
        similarity(&recording_identity(a).0, &recording_identity(b).0)
    } else {
        similarity(&a.title, &b.title)
    };
    let artist = if same_credits {
        1.
    } else {
        similarity(&a.artists.join(" "), &b.artists.join(" "))
    };
    let mut value = 0.25 * title + 0.20 * artist;
    let mut weight = 0.45;
    let mut compatible = versions_compatible(a, b) && title >= 0.6 && artist >= 0.6;
    if let (Some(x), Some(y)) = (&a.isrc, &b.isrc) {
        weight += 0.35;
        let same = normalize_isrc(x) == normalize_isrc(y);
        if same {
            value += 0.35;
        } else {
            compatible = false;
        }
    }
    if let (Some(x), Some(y)) = (a.duration_ms, b.duration_ms) {
        if x > 0 && y > 0 {
            let gap = x.abs_diff(y) as f64;
            value += 0.10 * (1. - gap / (x.max(y) as f64 * 0.10).max(2000.)).max(0.);
            weight += 0.10;
            if gap > 15_000. && gap / x.max(y) as f64 > 0.10 {
                compatible = false;
            }
        }
    }
    if let (Some(x), Some(y)) = (&a.album, &b.album) {
        weight += 0.10;
        value += 0.10 * similarity(x, y);
    }
    ((value / weight).clamp(0., 1.), compatible)
}

pub fn rank(track: &Metadata, candidates: &mut [PlaybackCandidate]) {
    for candidate in candidates.iter_mut() {
        (candidate.confidence, candidate.compatible) = score(track, &candidate.metadata);
    }
    candidates.sort_by(|a, b| {
        b.confidence
            .total_cmp(&a.confidence)
            .then_with(|| b.compatible.cmp(&a.compatible))
            .then_with(|| a.external_id.cmp(&b.external_id))
    });
}

/// Auto-selection requires recording evidence, never the displayed confidence alone.
/// Equivalent uploads are ranked deterministically; recording/version conflicts
/// still require review rather than accepting the displayed confidence alone.
pub fn automatic_candidate<'a>(
    track: &Metadata,
    candidates: &'a [PlaybackCandidate],
) -> Option<&'a PlaybackCandidate> {
    let mut selected: Option<&PlaybackCandidate> = None;
    for candidate in candidates {
        if candidate.provider != super::model::Platform::Youtube
            || !super::urls::video_id(&candidate.external_id)
            || !matches!((track.duration_ms, candidate.metadata.duration_ms), (Some(x), Some(y)) if x > 0 && y > 0)
            || normalize(&track.title).is_empty()
            || normalize(&track.artists.join(" ")).is_empty()
            || !same_recording(track, &candidate.metadata)
            || !score(track, &candidate.metadata).1
        {
            continue;
        }
        if let Some(previous) = selected {
            let order = score(track, &candidate.metadata)
                .0
                .total_cmp(&score(track, &previous.metadata).0)
                .then_with(|| {
                    let target = track.duration_ms.unwrap_or(0);
                    let current_gap = candidate.metadata.duration_ms.unwrap_or(0).abs_diff(target);
                    let previous_gap = previous.metadata.duration_ms.unwrap_or(0).abs_diff(target);
                    previous_gap.cmp(&current_gap)
                })
                .then_with(|| previous.external_id.cmp(&candidate.external_id));
            if order.is_le() {
                continue;
            }
        }
        selected = Some(candidate);
    }
    selected
}

/// Recovery tries the highest-scoring playable candidates for the current
/// session. These scores do not establish a verified library mapping.
pub fn autoplay_candidates(
    track: &Metadata,
    candidates: &[PlaybackCandidate],
) -> Vec<PlaybackCandidate> {
    let mut eligible: Vec<_> = candidates
        .iter()
        .filter(|candidate| {
            candidate.provider == super::model::Platform::Youtube
                && crate::youtube_native::resolver::valid_id(&candidate.external_id)
        })
        .cloned()
        .collect();
    rank(track, &mut eligible);
    let mut seen = std::collections::HashSet::new();
    eligible.retain(|candidate| {
        candidate.confidence > 0.0 && seen.insert(candidate.external_id.clone())
    });
    eligible.truncate(5);
    eligible
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::model::{fixture, Platform};
    #[test]
    fn autoplay_ranks_fallbacks_by_score_even_when_the_best_version_differs() {
        let mut target = fixture("source", "Trust").metadata;
        target.artists = vec!["Brent Faiyaz".into()];
        target.duration_ms = Some(158_420);
        target.album = Some("Original Album".into());
        let mut candidate = PlaybackCandidate {
            provider: Platform::Youtube,
            external_id: "vu1vQyjE1aA".into(),
            metadata: target.clone(),
            confidence: 0.0,
            compatible: false,
        };
        candidate.metadata.duration_ms = Some(171_000);
        candidate.metadata.album = Some("Other Release".into());
        let mut live = candidate.clone();
        live.external_id = "abcdefghijk".into();
        live.metadata.title = "Trust (Live)".into();
        live.metadata.duration_ms = target.duration_ms;
        live.metadata.album = target.album.clone();
        let mut invalid = live.clone();
        invalid.external_id = "invalid".into();
        let candidates = autoplay_candidates(
            &target,
            &[candidate.clone(), invalid, live.clone(), live.clone()],
        );
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].external_id, live.external_id);
        assert!(!candidates[0].compatible);
        assert!(candidates[0].confidence > candidates[1].confidence);
        assert_eq!(candidates[1].external_id, candidate.external_id);
    }
    #[test]
    fn featured_credits_can_move_between_title_and_artist_fields() {
        for (source_title, artists, duration, target_title, target_duration, album) in [
            (
                "Not You Too (feat. Chris Brown)",
                vec!["Drake", "Chris Brown"],
                269_680,
                "Not You Too (feat. Chris Brown)",
                270_000,
                "Dark Lane Demo Tapes",
            ),
            (
                "Fancy",
                vec!["Drake", "T.I.", "Swizz Beatz"],
                319_386,
                "Fancy (feat. T.I. & Swizz Beatz)",
                319_000,
                "R&B The Collection Summer 2011",
            ),
            (
                "Jimmy Cooks (feat. 21 Savage)",
                vec!["Drake", "21 Savage"],
                218_364,
                "Jimmy Cooks (feat. 21 Savage)",
                219_000,
                "Honestly, Nevermind",
            ),
        ] {
            let mut source = fixture("source", source_title).metadata;
            source.artists = artists.into_iter().map(String::from).collect();
            source.album = None;
            source.duration_ms = Some(duration);
            let mut target = source.clone();
            target.title = target_title.into();
            target.artists = vec!["Drake".into()];
            target.album = Some(album.into());
            target.duration_ms = Some(target_duration);
            assert!(same_recording(&source, &target), "{source_title}");
            assert!(score(&source, &target).1, "{source_title}");
            assert!(score(&source, &target).0 > 0.99, "{source_title}");
        }
    }

    #[test]
    fn album_names_are_not_track_version_labels() {
        let source = fixture("source", "Song").metadata;
        for album in ["Dark Lane Demo Tapes", "Live Through This"] {
            let mut target = source.clone();
            target.album = Some(album.into());
            assert!(versions_compatible(&source, &target), "{album}");
        }
    }

    #[test]
    fn unexplained_credits_and_long_duration_gaps_stay_in_review() {
        let mut source = fixture("source", "Paper Soldier (feat. Joony)").metadata;
        source.artists = vec![
            "Brent Faiyaz".into(),
            "Joony".into(),
            "ISO Supremacy".into(),
        ];
        source.duration_ms = Some(202_106);
        let mut target = source.clone();
        target.artists.pop();
        target.duration_ms = Some(203_000);
        assert!(!same_recording(&source, &target));
        for (title, gap) in [("Trust", 6500), ("Rolling Stone", 43_000)] {
            let mut source = fixture("source", title).metadata;
            source.duration_ms = Some(200_000);
            let mut target = source.clone();
            target.duration_ms = Some(200_000 + gap);
            assert!(!same_recording(&source, &target), "{title}");
        }
    }

    #[test]
    fn featured_credit_equivalence_preserves_recording_guards() {
        let mut source = fixture("source", "Fancy").metadata;
        source.artists = vec!["Drake".into(), "T.I.".into(), "Swizz Beatz".into()];
        let mut target = source.clone();
        target.title = "Fancy (feat. T.I. & Swizz Beatz)".into();
        target.artists = vec!["Drake".into()];
        assert!(same_recording(&source, &target));
        for title in [
            "Fancy (feat. Other Guest & Swizz Beatz)",
            "Fancy (feat. T.I.)",
            "Fancy (feat. T.I. & Swizz Beatz) (Live)",
            "Fancy (feat. T.I. & Swizz Beatz) (Remix)",
            "Fancy (feat. T.I. & Swizz Beatz) (Clean)",
            "Fancy (feat. T.I. & Swizz Beatz) (2020 Remaster)",
            "Fancy (feat. T.I. & Swizz Beatz) (Cover)",
        ] {
            let mut wrong = target.clone();
            wrong.title = title.into();
            assert!(!same_recording(&source, &wrong), "{title}");
        }
        let mut wrong = target.clone();
        wrong.artists = vec!["Other Singer".into()];
        assert!(!same_recording(&source, &wrong));
        wrong = target.clone();
        wrong.duration_ms = source.duration_ms.map(|x| x + 2001);
        assert!(!same_recording(&source, &wrong));
        wrong = target.clone();
        wrong.duration_ms = Some(0);
        let mut zero = source.clone();
        zero.duration_ms = Some(0);
        assert!(!same_recording(&zero, &wrong));
        wrong = target.clone();
        source.isrc = Some("USAAA1234567".into());
        wrong.isrc = Some("USBBB1234567".into());
        assert!(!same_recording(&source, &wrong));
        wrong = target;
        wrong.explicit = Some(false);
        source.explicit = Some(true);
        assert!(!same_recording(&source, &wrong));
    }
    #[test]
    fn automatic_match_recovers_poison_but_requires_unique_recording_evidence() {
        let mut original = fixture("spotify", "Poison").metadata;
        original.artists = vec!["Brent Faiyaz".into()];
        original.duration_ms = Some(212_992);
        original.album = None;
        let mut candidate = PlaybackCandidate {
            provider: Platform::Youtube,
            external_id: "fn0EIOnK-D0".into(),
            metadata: original.clone(),
            confidence: 0.999931,
            compatible: true,
        };
        candidate.metadata.duration_ms = Some(213_000);
        assert!(automatic_candidate(&original, &[candidate.clone()]).is_some());
        let mut alternate = candidate.clone();
        alternate.external_id = "O8CKxlmkZtw".into();
        alternate.metadata.duration_ms = Some(306_000);
        assert!(automatic_candidate(&original, &[candidate.clone(), alternate.clone()]).is_some());
        alternate.metadata.duration_ms = candidate.metadata.duration_ms;
        let equivalent = [candidate.clone(), alternate];
        assert!(
            automatic_candidate(&original, &equivalent).is_some(),
            "duplicate uploads with the same recording evidence should not block playback"
        );
        for title in ["Poison (Live)", "Poison (Remix)", "Poison (Cover)"] {
            let mut wrong = candidate.clone();
            wrong.metadata.title = title.into();
            assert!(automatic_candidate(&original, &[wrong]).is_none());
        }
        let mut wrong = candidate.clone();
        wrong.metadata.artists = vec!["Other artist".into()];
        assert!(automatic_candidate(&original, &[wrong]).is_none());
        let mut wrong = candidate.clone();
        wrong.metadata.duration_ms = Some(0);
        assert!(automatic_candidate(&original, &[wrong]).is_none());
        let mut identified = original.clone();
        identified.isrc = Some("USAAA1234567".into());
        candidate.metadata.isrc = Some("USBBB1234567".into());
        assert!(automatic_candidate(&identified, &[candidate.clone()]).is_none());
        candidate.metadata.isrc = None;
        candidate.metadata.explicit = Some(true);
        identified.explicit = Some(false);
        assert!(automatic_candidate(&identified, &[candidate]).is_none());
    }
    #[test]
    fn normalization_preserves_version_markers_and_original_strings() {
        assert_eq!(normalize("Ｃａｆé — Song (feat. A)"), "café song feat a");
        assert_eq!(normalize("Cafe\u{301}"), "café");
        let m = fixture("a", "Song (Live, 2020 Remaster)").metadata;
        assert!(!versions_compatible(&m, &fixture("b", "Song").metadata));
        assert_eq!(m.title, "Song (Live, 2020 Remaster)");
    }
    #[test]
    fn scoring_rejects_covers_and_incompatible_recordings_even_with_same_identifier() {
        let a = fixture("a", "Song").metadata;
        for version in [
            "Song (Live)",
            "Song (Remix)",
            "Song (Acoustic)",
            "Song (Instrumental)",
            "Song (Remastered)",
            "Song (Clean)",
        ] {
            let mut b = fixture("b", version).metadata;
            b.isrc = Some("USAAA1234567".into());
            let mut original = a.clone();
            original.isrc = b.isrc.clone();
            let score = score(&original, &b);
            assert!(!score.1, "{version}");
        }
        let mut cover = a.clone();
        cover.artists = vec!["A different singer".into()];
        assert!(!score(&a, &cover).1);
        assert!(!same_recording(&a, &cover));
    }
    #[test]
    fn confidence_is_a_bounded_heuristic_with_missing_fields_not_an_auto_match() {
        let a = fixture("a", "Song").metadata;
        assert!((score(&a, &a).0 - 1.).abs() < 0.001);
        let mut wrong = a.clone();
        wrong.title = "Another song entirely".into();
        assert!(score(&a, &wrong).0 < score(&a, &a).0);
        let mut candidates = vec![
            PlaybackCandidate {
                provider: Platform::Youtube,
                external_id: "abcdefghijk".into(),
                metadata: wrong,
                confidence: 0.,
                compatible: true,
            },
            PlaybackCandidate {
                provider: Platform::Youtube,
                external_id: "12345678901".into(),
                metadata: a.clone(),
                confidence: 0.,
                compatible: true,
            },
        ];
        rank(&a, &mut candidates);
        assert_eq!(candidates[0].external_id, "12345678901");
        assert!(candidates
            .iter()
            .all(|c| (0.0..=1.0).contains(&c.confidence)));
    }
    #[test]
    fn explicit_editions_and_conflicting_isrcs_cannot_merge() {
        let mut a = fixture("a", "Song").metadata;
        let mut b = a.clone();
        a.explicit = Some(true);
        b.explicit = Some(false);
        assert!(!same_recording(&a, &b));
        b.explicit = a.explicit;
        a.isrc = Some("USAAA1234567".into());
        b.isrc = Some("USBBB1234567".into());
        assert!(!score(&a, &b).1);
    }
}
