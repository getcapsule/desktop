use serde::{Deserialize, Serialize};

use crate::db::SongLookup;

const USER_AGENT: &str = concat!(
    "capsule/",
    env!("CARGO_PKG_VERSION"),
    " (+https://lrclib.net)"
);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LrclibHit {
    #[serde(default)]
    track_name: String,
    #[serde(default)]
    artist_name: String,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    plain_lyrics: Option<String>,
    #[serde(default)]
    synced_lyrics: Option<String>,
}

impl LrclibHit {
    fn has_words(&self) -> bool {
        self.synced_lyrics
            .as_ref()
            .is_some_and(|s| !s.trim().is_empty())
            || self
                .plain_lyrics
                .as_ref()
                .is_some_and(|s| !s.trim().is_empty())
    }

    fn has_synced(&self) -> bool {
        self.synced_lyrics
            .as_ref()
            .is_some_and(|s| !s.trim().is_empty())
    }

    fn into_fetched(self) -> Fetched {
        Fetched {
            synced: self.synced_lyrics.filter(|s| !s.trim().is_empty()),
            plain: self.plain_lyrics.filter(|s| !s.trim().is_empty()),
        }
    }
}

fn pick_best(hits: Vec<LrclibHit>, song: &SongLookup) -> Option<LrclibHit> {
    let want = song.duration_ms as f64 / 1000.0;
    hits.into_iter()
        .filter(LrclibHit::has_words)
        .filter(|hit| matches_song(hit, song))
        .min_by(|a, b| {
            delta(a, want)
                .total_cmp(&delta(b, want))
                .then_with(|| b.has_synced().cmp(&a.has_synced()))
        })
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn matches_song(hit: &LrclibHit, song: &SongLookup) -> bool {
    let title = normalized(&song.name);
    let artist = normalized(&song.artist_name);
    if title.is_empty()
        || artist.is_empty()
        || normalized(&hit.track_name) != title
        || normalized(&hit.artist_name) != artist
    {
        return false;
    }
    // Unknown lengths may only match an otherwise identical title and artist.
    song.duration_ms == 0
        || hit.duration.is_some_and(|d| {
            d.is_finite() && d > 0.0 && (d - song.duration_ms as f64 / 1000.0).abs() <= 4.0
        })
}

fn delta(hit: &LrclibHit, want: f64) -> f64 {
    hit.duration.map_or(f64::MAX, |d| (d - want).abs())
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fetched {
    pub synced: Option<String>,
    pub plain: Option<String>,
}

pub async fn fetch(song: &SongLookup) -> Result<Fetched, Box<dyn std::error::Error + Send + Sync>> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::Client, reqwest::Error>> =
        std::sync::OnceLock::new();
    let client = CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(12))
                .build()
        })
        .as_ref()
        .map_err(|e| e.to_string())?;
    if normalized(&song.name).is_empty() || normalized(&song.artist_name).is_empty() {
        return Ok(Fetched::default());
    }

    // A failing exact-match endpoint should not prevent the independent search
    // endpoint from supplying lyrics. Never cache a transport failure as a miss.
    if let Ok(Some(hit)) = get_exact(client, song).await {
        return Ok(hit.into_fetched());
    }

    Ok(search(client, song)
        .await?
        .map(LrclibHit::into_fetched)
        .unwrap_or_default())
}

async fn get_exact(
    client: &reqwest::Client,
    song: &SongLookup,
) -> Result<Option<LrclibHit>, Box<dyn std::error::Error + Send + Sync>> {
    let url = reqwest::Url::parse_with_params(
        "https://lrclib.net/api/get",
        &[
            ("artist_name", song.artist_name.as_str()),
            ("track_name", song.name.as_str()),
            ("album_name", song.album_name.as_str()),
            ("duration", &(song.duration_ms / 1000).to_string()),
        ],
    )?;

    let resp = client.get(url).send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("lrclib get {}", resp.status()).into());
    }

    let hit: LrclibHit = read_json(resp).await?;
    Ok(Some(hit).filter(|hit| hit.has_words() && matches_song(hit, song)))
}

async fn search(
    client: &reqwest::Client,
    song: &SongLookup,
) -> Result<Option<LrclibHit>, Box<dyn std::error::Error + Send + Sync>> {
    let url = reqwest::Url::parse_with_params(
        "https://lrclib.net/api/search",
        &[
            ("artist_name", song.artist_name.as_str()),
            ("track_name", song.name.as_str()),
        ],
    )?;

    let resp = client.get(url).send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("lrclib search {}", resp.status()).into());
    }

    let hits: Vec<LrclibHit> = read_json(resp).await?;
    Ok(pick_best(hits, song))
}

async fn read_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T, Box<dyn std::error::Error + Send + Sync>> {
    const LIMIT: usize = 2 * 1024 * 1024;
    if response.content_length().is_some_and(|n| n > LIMIT as u64) {
        return Err("lyrics response too large".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > LIMIT {
            return Err("lyrics response too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub at_ms: u64,
    pub text: String,
}

pub fn parse_lrc(body: &str) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();

    for raw in body.lines() {
        let mut rest = raw.trim();
        let mut stamps: Vec<u64> = Vec::new();

        while rest.starts_with('[') {
            let Some(close) = rest.find(']') else { break };
            let tag = &rest[1..close];
            match parse_stamp(tag) {
                Some(ms) => stamps.push(ms),
                None => break,
            }
            rest = rest[close + 1..].trim_start();
        }

        if stamps.is_empty() {
            continue;
        }
        let text = rest.trim().to_string();
        for at_ms in stamps {
            out.push(Line {
                at_ms,
                text: text.clone(),
            });
        }
    }

    out.sort_by_key(|l| l.at_ms);
    out
}

fn parse_stamp(tag: &str) -> Option<u64> {
    let (mins, rest) = tag.split_once(':')?;
    let mins: u64 = mins.trim().parse().ok()?;

    let (secs, frac) = match rest.split_once(['.', ':']) {
        Some((s, f)) => (s, Some(f)),
        None => (rest, None),
    };
    let secs: u64 = secs.trim().parse().ok()?;
    if secs >= 60 {
        return None;
    }

    let millis = match frac {
        None => 0,
        Some(f) => {
            let digits: String = f.chars().take_while(char::is_ascii_digit).take(3).collect();
            if digits.is_empty() {
                return None;
            }
            let v: u64 = digits.parse().ok()?;
            match digits.len() {
                1 => v * 100,
                2 => v * 10,
                _ => v,
            }
        }
    };

    mins.checked_mul(60)?
        .checked_add(secs)?
        .checked_mul(1000)?
        .checked_add(millis)
}

pub fn active_line(lines: &[Line], position_ms: u64) -> Option<usize> {
    if lines.is_empty() || position_ms < lines[0].at_ms {
        return None;
    }
    match lines.binary_search_by_key(&position_ms, |l| l.at_ms) {
        Ok(i) => {
            let mut i = i;
            while i + 1 < lines.len() && lines[i + 1].at_ms == position_ms {
                i += 1;
            }
            Some(i)
        }
        Err(i) => Some(i - 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires public LRCLIB network access"]
    async fn public_import_metadata_finds_lyrics() {
        let result = fetch(&SongLookup {
            name: "Poison".into(),
            artist_name: "Brent Faiyaz".into(),
            album_name: String::new(),
            duration_ms: 212_992,
        })
        .await
        .unwrap();
        assert!(result.synced.is_some() || result.plain.is_some());
        println!(
            "Imported metadata lyrics lookup passed; synced={}",
            result.synced.is_some()
        );
    }
    #[test]
    fn timestamp_precision_truncates_and_overflow_is_rejected() {
        assert_eq!(parse_lrc("[00:01.12345]words")[0].at_ms, 1123);
        assert!(parse_lrc("[18446744073709551615:59.999]invalid").is_empty());
    }

    #[test]
    fn parses_timestamps_at_each_precision() {
        let lines = parse_lrc("[00:01]one\n[00:02.5]two\n[00:03.25]three\n[01:00.500]four");
        assert_eq!(
            lines.iter().map(|l| l.at_ms).collect::<Vec<_>>(),
            vec![1_000, 2_500, 3_250, 60_500]
        );
    }

    #[test]
    fn metadata_tags_are_not_timestamps() {
        let lines = parse_lrc("[ar:D3]\n[ti:Yalan]\n[length:03:20]\n[00:10.00]real line");
        assert_eq!(
            lines,
            vec![Line {
                at_ms: 10_000,
                text: "real line".into()
            }]
        );
    }

    #[test]
    fn repeated_timestamps_expand_to_one_line_each() {
        let lines = parse_lrc("[00:12.00][01:04.00]chorus");
        assert_eq!(
            lines,
            vec![
                Line {
                    at_ms: 12_000,
                    text: "chorus".into()
                },
                Line {
                    at_ms: 64_000,
                    text: "chorus".into()
                },
            ]
        );
    }

    #[test]
    fn empty_timed_lines_are_kept_as_instrumental_gaps() {
        let lines = parse_lrc("[00:05.00]sing\n[00:09.00]\n[00:20.00]again");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].text, "");
    }

    #[test]
    fn output_is_sorted_even_when_the_file_is_not() {
        let lines = parse_lrc("[00:30.00]later\n[00:10.00]earlier");
        assert_eq!(lines[0].text, "earlier");
        assert_eq!(lines[1].text, "later");
    }

    #[test]
    fn junk_and_untimed_lines_are_dropped() {
        assert!(parse_lrc("no tags here\n[not a stamp]\n[99:99.99]bad seconds").is_empty());
    }

    #[test]
    fn active_line_tracks_position() {
        let lines = parse_lrc("[00:05.00]a\n[00:10.00]b\n[00:20.00]c");

        assert_eq!(active_line(&lines, 0), None, "intro has no active line");
        assert_eq!(active_line(&lines, 4_999), None);
        assert_eq!(active_line(&lines, 5_000), Some(0), "exact hit");
        assert_eq!(active_line(&lines, 9_999), Some(0));
        assert_eq!(active_line(&lines, 10_000), Some(1));
        assert_eq!(
            active_line(&lines, 999_999),
            Some(2),
            "past the end holds the last"
        );
    }

    fn song(duration_ms: u64) -> SongLookup {
        SongLookup {
            name: "Song".into(),
            artist_name: "Artist".into(),
            album_name: "Album".into(),
            duration_ms,
        }
    }

    fn hit(duration: f64, synced: bool, plain: bool) -> LrclibHit {
        LrclibHit {
            track_name: "Song".into(),
            artist_name: "Artist".into(),
            duration: Some(duration),
            synced_lyrics: synced.then(|| "[00:01.00]x".to_string()),
            plain_lyrics: plain.then(|| "x".to_string()),
        }
    }

    #[test]
    fn best_match_rejects_distant_synced_version() {
        let picked = pick_best(
            vec![hit(200.0, false, true), hit(215.0, true, false)],
            &song(200_000),
        );
        assert!(!picked.expect("a match").has_synced());
    }

    #[test]
    fn best_match_breaks_ties_on_duration() {
        let picked = pick_best(
            vec![
                hit(180.0, true, true),
                hit(271.0, true, true),
                hit(400.0, true, true),
            ],
            &song(272_000),
        );
        assert_eq!(picked.expect("a match").duration, Some(271.0));
    }

    #[test]
    fn best_match_ignores_entries_with_no_words() {
        assert!(pick_best(vec![hit(272.0, false, false)], &song(272_000)).is_none());
        assert!(pick_best(vec![], &song(272_000)).is_none());
    }

    #[test]
    fn best_match_tolerates_a_missing_duration() {
        let mut unknown = hit(0.0, true, true);
        unknown.duration = None;
        let picked = pick_best(vec![unknown, hit(272.0, true, true)], &song(272_000));
        assert_eq!(picked.expect("a match").duration, Some(272.0));
    }

    #[test]
    fn search_must_not_accept_missing_or_unrelated_metadata() {
        let unrelated: LrclibHit = serde_json::from_str(r#"{"trackName":"Different song","artistName":"Someone else","duration":200,"syncedLyrics":"[00:01]wrong"}"#).unwrap();
        assert!(pick_best(vec![unrelated], &song(200_000)).is_none());
    }

    #[test]
    fn active_line_handles_no_lyrics() {
        assert_eq!(active_line(&[], 1_000), None);
    }
}
