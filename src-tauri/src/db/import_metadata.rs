use super::*;

impl Db {
    pub fn spotify_metadata_gaps(&self) -> Result<Vec<(String, String)>, DbError> {
        // Track embeds do not always expose an album. Once artwork is known,
        // avoid fetching the same unfillable album metadata on every launch.
        let mut statement = self.conn.prepare("SELECT t.id,MIN(o.external_id) FROM tracks t JOIN track_origins o ON o.track_id=t.id WHERE o.platform='spotify' AND NULLIF(TRIM(t.artwork_url),'') IS NULL GROUP BY t.id ORDER BY t.created_at,t.id LIMIT 10000")?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn fill_spotify_metadata(
        &self,
        track: &str,
        spotify_id: &str,
        detail: &crate::import::model::Metadata,
    ) -> Result<bool, DbError> {
        let known: bool = self.conn.query_row("SELECT EXISTS(SELECT 1 FROM track_origins WHERE track_id=?1 AND platform='spotify' AND external_id=?2)",params![track,spotify_id],|row|row.get(0))?;
        if !known {
            return Err(DbError::Import(
                "Spotify origin no longer belongs to this track.".into(),
            ));
        }
        let Some(current) = self.canonical_track(track)? else {
            return Ok(false);
        };
        let mut merged = current.metadata.clone();
        crate::import::public_spotify::merge_missing_metadata(&mut merged, detail.clone());
        let album = merged.album;
        let artwork = merged.artwork_url;
        let isrc = merged.isrc;
        let duration = merged.duration_ms;
        if album == current.metadata.album
            && artwork == current.metadata.artwork_url
            && isrc == current.metadata.isrc
            && duration == current.metadata.duration_ms
        {
            return Ok(false);
        }
        Ok(self.conn.execute("UPDATE tracks SET album=?2,artwork_url=?3,isrc=?4,duration_ms=?5,updated_at=CURRENT_TIMESTAMP WHERE id=?1",params![track,album,artwork,isrc,duration])? > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{
        model::{fixture, ImportedCollection, Platform},
        urls::{detect, ContentKind},
    };
    #[test]
    fn saved_spotify_tracks_repair_missing_fields_without_replacing_known_metadata() {
        let mut db = Db::open_in_memory().unwrap();
        let mut song = fixture("5NijSs5dAwaIybq1GaRTIe", "Poison");
        song.origin.platform = Platform::Spotify;
        song.metadata.album = None;
        song.metadata.artwork_url = None;
        let source = "https://open.spotify.com/track/5NijSs5dAwaIybq1GaRTIe";
        let collection = ImportedCollection {
            source_url: source.into(),
            provider: Platform::Spotify,
            kind: ContentKind::Track,
            title: "Poison".into(),
            artwork_url: None,
            tracks: vec![song.clone()],
            warnings: vec![],
        };
        let job = db.create_import_job(&detect(source).unwrap()).unwrap();
        db.set_import_preview(&job.id, &collection).unwrap();
        db.confirm_import(&job.id).unwrap();
        let gaps = db.spotify_metadata_gaps().unwrap();
        assert_eq!(gaps.len(), 1);
        let id = &gaps[0].0;
        let mut detail = song.metadata.clone();
        detail.title = "Provider title changed".into();
        detail.album = Some("A.M. Paradox".into());
        detail.artwork_url = Some("https://i.scdn.co/image/test-cover".into());
        detail.duration_ms = Some(999);
        assert!(db.fill_spotify_metadata(id, &gaps[0].1, &detail).unwrap());
        let repaired = db.canonical_track(id).unwrap().unwrap();
        assert_eq!(repaired.metadata.title, "Poison");
        assert_eq!(repaired.metadata.duration_ms, song.metadata.duration_ms);
        assert_eq!(repaired.metadata.artwork_url, detail.artwork_url);
        assert_eq!(repaired.metadata.album, detail.album);
        assert!(db.spotify_metadata_gaps().unwrap().is_empty());
        assert!(!db.fill_spotify_metadata(id, &gaps[0].1, &detail).unwrap());
        assert!(db
            .fill_spotify_metadata(id, "wrong-origin", &detail)
            .is_err());
        db.conn.execute("UPDATE tracks SET album=NULL WHERE id=?1",[id]).unwrap();
        assert!(db.spotify_metadata_gaps().unwrap().is_empty(), "unknown album must not trigger repeated artwork requests");
    }
}
