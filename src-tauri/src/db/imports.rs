use super::*;
use crate::import::model::*;
use uuid::Uuid;

fn invalid(message: impl Into<String>) -> DbError {
    DbError::Import(message.into())
}

fn json<T: Serialize>(value: &T) -> Result<String, DbError> {
    serde_json::to_string(value).map_err(|_| invalid("Could not encode library metadata."))
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, DbError> {
    serde_json::from_str(value).map_err(|_| invalid("Stored import data is invalid."))
}

impl Db {
    /// Persist a searched source recording without creating an import playlist.
    pub fn save_source_track(&mut self, imported: &ImportedTrack) -> Result<CapsuleTrack, DbError> {
        validate_metadata(imported)?;
        let origin = &imported.origin;
        let m = &imported.metadata;
        let tx = self.conn.transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT track_id FROM track_origins WHERE platform=?1 AND external_id=?2",
                params![origin.platform.as_str(), origin.external_id],
                |row| row.get(0),
            )
            .optional()?;
        let id = existing.unwrap_or_else(|| Uuid::new_v4().to_string());
        tx.execute("INSERT INTO tracks(id,title,artists,album,duration_ms,artwork_url,isrc,explicit) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO NOTHING",params![id,m.title,json(&m.artists)?,m.album,m.duration_ms,m.artwork_url,m.isrc,m.explicit])?;
        tx.execute("INSERT INTO track_origins(track_id,platform,external_id,url,metadata_json) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(platform,external_id) DO UPDATE SET url=excluded.url,metadata_json=excluded.metadata_json",params![id,origin.platform.as_str(),origin.external_id,origin.url,json(m)?])?;
        tx.execute("INSERT INTO track_resolution(track_id,state) VALUES(?1,'matched') ON CONFLICT DO NOTHING", [&id])?;
        tx.commit()?;
        self.canonical_track(&id)?
            .ok_or_else(|| invalid("Could not save source recording."))
    }
    pub fn contains_song(&self, id: &str) -> Result<bool, DbError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM songs WHERE id=?1)",
            [id],
            |r| r.get(0),
        )?)
    }
    pub fn attach_playback_origin(&self, track: &str, origin: &Origin) -> Result<(), DbError> {
        if self.canonical_track(track)?.is_none() {
            return Err(invalid("Unknown imported track."));
        }
        if let Some(existing) =
            self.find_track_by_origin(origin.platform.as_str(), &origin.external_id)?
        {
            if existing != track {
                return Err(invalid(
                    "That source is already linked to another canonical track.",
                ));
            }
        }
        self.conn.execute("INSERT INTO track_origins(track_id,platform,external_id,url) VALUES(?1,?2,?3,?4) ON CONFLICT(platform,external_id) DO UPDATE SET url=excluded.url",params![track,origin.platform.as_str(),origin.external_id,origin.url])?;
        Ok(())
    }
    pub fn source_preference(&self, track: &str) -> Result<Option<Platform>, DbError> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT provider FROM track_source_preferences WHERE track_id=?1",
                [track],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|v| decode(&json(&v)?)).transpose()
    }
    pub fn set_source_preference(
        &self,
        track: &str,
        provider: Option<Platform>,
    ) -> Result<(), DbError> {
        if let Some(provider) = provider {
            self.conn.execute("INSERT INTO track_source_preferences(track_id,provider) VALUES(?1,?2) ON CONFLICT(track_id) DO UPDATE SET provider=excluded.provider",params![track,provider.as_str()])?;
        } else {
            self.conn.execute(
                "DELETE FROM track_source_preferences WHERE track_id=?1",
                [track],
            )?;
        }
        Ok(())
    }
    pub fn create_import_job(
        &mut self,
        target: &crate::import::urls::ImportUrl,
    ) -> Result<ImportJobRow, DbError> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute("INSERT INTO import_jobs(id,source_url,provider,collection_type,status) VALUES(?1,?2,?3,?4,'fetching')", params![id,target.url,target.provider.as_str(),json(&target.kind)?.trim_matches('"')])?;
        self.get_import_job(&id)?
            .ok_or_else(|| invalid("Import job was not created."))
    }
    pub fn set_import_preview(
        &self,
        job: &str,
        collection: &ImportedCollection,
    ) -> Result<(), DbError> {
        let mut collection = collection.clone();
        collection.tracks.retain(|track| {
            if validate_metadata(track).is_ok() {
                true
            } else {
                collection
                    .warnings
                    .push("An entry with invalid metadata was skipped.".into());
                false
            }
        });
        let changed = self.conn.execute("UPDATE import_jobs SET status='ready',preview_json=?2,collection_title=?3,collection_artwork=?4,total_tracks=?5,unavailable=?6,skipped_tracks=?6,updated_at=CURRENT_TIMESTAMP WHERE id=?1 AND status='fetching'", params![job,json(&collection)?,collection.title,collection.artwork_url,collection.tracks.len(),collection.warnings.len()])?;
        if changed == 0 {
            return Err(invalid("Import is cancelled or no longer fetching."));
        }
        Ok(())
    }
    pub fn import_preview(&self, job: &str) -> Result<Option<ImportedCollection>, DbError> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT preview_json FROM import_jobs WHERE id=?1",
                [job],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        raw.map(|v| decode(&v)).transpose()
    }
    pub fn import_job_playlist(&self, job: &str) -> Result<Option<String>, DbError> {
        Ok(self
            .conn
            .query_row(
                "SELECT playlist_id FROM import_jobs WHERE id=?1",
                [job],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }
    pub fn list_import_jobs(&self) -> Result<Vec<ImportJobRow>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM import_jobs ORDER BY created_at DESC,rowid DESC LIMIT 100")?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.iter()
            .map(|id| {
                self.get_import_job(id)?
                    .ok_or_else(|| invalid("Missing import job."))
            })
            .collect()
    }
    pub fn fail_import(&self, job: &str, error: &str) -> Result<(), DbError> {
        self.conn.execute("UPDATE import_jobs SET status='error',error=?2,updated_at=CURRENT_TIMESTAMP WHERE id=?1 AND status IN ('fetching','resolving')",params![job,error])?;
        Ok(())
    }
    pub fn cancel_import(&self, job: &str) -> Result<(), DbError> {
        self.conn.execute("UPDATE import_jobs SET status='cancelled',updated_at=CURRENT_TIMESTAMP WHERE id=?1 AND status NOT IN ('completed','cancelled')",[job])?;
        Ok(())
    }
    pub fn recover_imports(&self) -> Result<(), DbError> {
        self.conn.execute_batch("UPDATE import_jobs SET status='interrupted',updated_at=CURRENT_TIMESTAMP WHERE status IN ('fetching','resolving'); UPDATE track_resolution SET state='pending',updated_at=CURRENT_TIMESTAMP WHERE state='searching';")?;
        Ok(())
    }
    pub fn confirm_import(&mut self, job: &str) -> Result<String, DbError> {
        if let Some(playlist) = self.import_job_playlist(job)? {
            return Ok(playlist);
        }
        let info = self
            .get_import_job(job)?
            .ok_or_else(|| invalid("Unknown import job."))?;
        if info.status != "ready" {
            return Err(invalid("Wait for a preview before confirming this import."));
        }
        let collection = self
            .import_preview(job)?
            .ok_or_else(|| invalid("Missing import preview."))?;
        if collection.tracks.len() > 100_000 || collection.tracks.is_empty() {
            return Err(invalid(
                "The collection has no available tracks or exceeds the import limit.",
            ));
        }
        let playlist = Uuid::new_v4().to_string();
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO playlists(id,name,artwork_url,can_edit,is_local,source_url) VALUES(?1,?2,?3,1,1,?4)",params![playlist,collection.title,collection.artwork_url,collection.source_url])?;
        for (position, imported) in collection.tracks.iter().enumerate() {
            validate_metadata(imported)?;
            let origin = &imported.origin;
            let m = &imported.metadata;
            let existing: Option<String> = tx
                .query_row(
                    "SELECT track_id FROM track_origins WHERE platform=?1 AND external_id=?2",
                    params![origin.platform.as_str(), origin.external_id],
                    |r| r.get(0),
                )
                .optional()?;
            let isrc = m
                .isrc
                .as_deref()
                .map(crate::import::matching::normalize_isrc)
                .filter(|s| !s.is_empty());
            let mut id = existing;
            if id.is_none() {
                if let Some(isrc) = &isrc {
                    let mut stmt = tx.prepare("SELECT id FROM tracks WHERE isrc=?1")?;
                    let candidates = stmt
                        .query_map([isrc], |r| r.get::<_, String>(0))?
                        .collect::<Result<Vec<_>, _>>()?;
                    for candidate in candidates {
                        if read_track(&tx, &candidate)?.is_some_and(|t| {
                            crate::import::matching::same_recording(&t.metadata, m)
                        }) {
                            id = Some(candidate);
                            break;
                        }
                    }
                }
            }
            let id = id.unwrap_or_else(|| Uuid::new_v4().to_string());
            tx.execute("INSERT INTO tracks(id,title,artists,album,duration_ms,artwork_url,isrc,explicit) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO NOTHING",params![id,m.title,json(&m.artists)?,m.album,m.duration_ms,m.artwork_url,isrc,m.explicit])?;
            tx.execute("INSERT INTO track_origins(track_id,platform,external_id,url,metadata_json) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(platform,external_id) DO UPDATE SET url=excluded.url,metadata_json=excluded.metadata_json",params![id,origin.platform.as_str(),origin.external_id,origin.url,json(m)?])?;
            tx.execute(
                "INSERT INTO track_resolution(track_id) VALUES(?1) ON CONFLICT DO NOTHING",
                [&id],
            )?;
            if origin.platform == Platform::Youtube
                && crate::import::urls::video_id(&origin.external_id)
            {
                tx.execute("INSERT INTO playback_mappings(track_id,provider,external_id,confidence,verified) VALUES(?1,'youtube',?2,1,1) ON CONFLICT DO NOTHING",params![id,origin.external_id])?;
                tx.execute("UPDATE track_resolution SET state='matched' WHERE track_id=?1 AND EXISTS(SELECT 1 FROM playback_mappings m WHERE m.track_id=?1 AND m.provider='youtube' AND m.verified=1)",[&id])?;
            }
            tx.execute("INSERT INTO playlist_tracks(playlist_id,position,song_id,entry_id) VALUES(?1,?2,?3,?4)",params![playlist,position,id,Uuid::new_v4().to_string()])?;
        }
        tx.execute("UPDATE import_jobs SET playlist_id=?2,status='resolving',processed=0,preview_json=NULL,updated_at=CURRENT_TIMESTAMP WHERE id=?1",params![job,playlist])?;
        tx.commit()?;
        Ok(playlist)
    }
    pub fn canonical_track(&self, id: &str) -> Result<Option<CapsuleTrack>, DbError> {
        read_track(&self.conn, id)
    }
    pub fn import_entries(&self, playlist: &str) -> Result<Vec<PlaylistEntry>, DbError> {
        let mut stmt = self.conn.prepare("SELECT p.entry_id,p.position,p.song_id,COALESCE(r.state,'pending') FROM playlist_tracks p JOIN playlists l ON l.id=p.playlist_id LEFT JOIN track_resolution r ON r.track_id=p.song_id WHERE l.is_local=1 AND p.playlist_id=?1 ORDER BY p.position")?;
        let records = stmt
            .query_map([playlist], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u32>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        records
            .into_iter()
            .map(|(entry_id, position, id, resolution)| {
                Ok(PlaylistEntry {
                    entry_id,
                    position,
                    track: self
                        .canonical_track(&id)?
                        .ok_or_else(|| invalid("Missing canonical track."))?,
                    resolution,
                    source_preference: self.source_preference(&id)?,
                })
            })
            .collect()
    }
    pub fn rename_import_playlist(&self, playlist: &str, name: &str) -> Result<(), DbError> {
        if name.trim().is_empty() || name.len() > 1000 {
            return Err(invalid("Enter a playlist name up to 1000 characters."));
        }
        if self.conn.execute(
            "UPDATE playlists SET name=?2 WHERE id=?1 AND is_local=1",
            params![playlist, name],
        )? == 0
        {
            return Err(invalid("Unknown local playlist."));
        }
        Ok(())
    }
    pub fn reorder_import_playlist(
        &mut self,
        playlist: &str,
        ids: &[String],
    ) -> Result<(), DbError> {
        let entries = self.import_entries(playlist)?;
        let expected: HashSet<&str> = entries.iter().map(|e| e.entry_id.as_str()).collect();
        let actual: HashSet<&str> = ids.iter().map(String::as_str).collect();
        if entries.len() != ids.len() || actual.len() != ids.len() || expected != actual {
            return Err(invalid(
                "Reordering must contain every playlist entry exactly once.",
            ));
        }
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE playlist_tracks SET position=-position-1 WHERE playlist_id=?1",
            [playlist],
        )?;
        for (position, id) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE playlist_tracks SET position=?3 WHERE playlist_id=?1 AND entry_id=?2",
                params![playlist, id, position],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn remove_import_entry(&mut self, playlist: &str, entry: &str) -> Result<(), DbError> {
        let mut ids: Vec<_> = self
            .import_entries(playlist)?
            .into_iter()
            .map(|e| e.entry_id)
            .collect();
        if !ids.iter().any(|id| id == entry) {
            return Err(invalid("Unknown playlist entry."));
        }
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM playlist_tracks WHERE playlist_id=?1 AND entry_id=?2",
            params![playlist, entry],
        )?;
        ids.retain(|id| id != entry);
        tx.execute(
            "UPDATE playlist_tracks SET position=-position-1 WHERE playlist_id=?1",
            [playlist],
        )?;
        for (position, id) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE playlist_tracks SET position=?3 WHERE playlist_id=?1 AND entry_id=?2",
                params![playlist, id, position],
            )?;
        }
        tx.commit()?;
        self.refresh_track_jobs_for_playlist(playlist)
    }
    fn refresh_track_jobs_for_playlist(&self, playlist: &str) -> Result<(), DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM import_jobs WHERE playlist_id=?1")?;
        let ids = stmt
            .query_map([playlist], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for id in ids {
            self.update_import_progress(&id)?;
        }
        Ok(())
    }
    pub fn resolution_candidates(&self, track: &str) -> Result<Vec<PlaybackCandidate>, DbError> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT candidates_json FROM track_resolution WHERE track_id=?1",
                [track],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|v| decode(&v))
            .transpose()
            .map(Option::unwrap_or_default)
    }
    pub fn resolution_state(&self, track: &str) -> Result<Option<String>, DbError> {
        Ok(self
            .conn
            .query_row(
                "SELECT state FROM track_resolution WHERE track_id=?1",
                [track],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn set_resolution(
        &self,
        track: &str,
        state: &str,
        candidates: &[PlaybackCandidate],
        error: Option<&str>,
    ) -> Result<(), DbError> {
        if !matches!(
            state,
            "pending" | "searching" | "matched" | "needs_review" | "unavailable" | "error"
        ) {
            return Err(invalid("Unknown matching state."));
        }
        self.conn.execute("INSERT INTO track_resolution(track_id,state,candidates_json,error) VALUES(?1,?2,?3,?4) ON CONFLICT(track_id) DO UPDATE SET state=excluded.state,candidates_json=excluded.candidates_json,error=excluded.error,updated_at=CURRENT_TIMESTAMP",params![track,state,json(&candidates)?,error])?;
        self.refresh_track_jobs(track)
    }
    pub fn cached_candidates(
        &self,
        query: &str,
    ) -> Result<Option<Vec<PlaybackCandidate>>, DbError> {
        let raw: Option<String>=self.conn.query_row("SELECT candidates_json FROM resolution_cache WHERE query=?1 AND expires_at>unixepoch()",[query],|r|r.get(0)).optional()?;
        raw.map(|v| decode(&v)).transpose()
    }
    pub fn cache_candidates(
        &self,
        query: &str,
        candidates: &[PlaybackCandidate],
    ) -> Result<(), DbError> {
        self.conn.execute("INSERT INTO resolution_cache(query,candidates_json,expires_at) VALUES(?1,?2,unixepoch()+86400) ON CONFLICT(query) DO UPDATE SET candidates_json=excluded.candidates_json,expires_at=excluded.expires_at",params![query,json(&candidates)?])?;
        self.conn.execute("DELETE FROM resolution_cache WHERE query IN (SELECT query FROM resolution_cache ORDER BY expires_at DESC LIMIT -1 OFFSET 1000)",[])?;
        Ok(())
    }
    /// Upgrade old review results on demand without changing manual or invalidated choices.
    pub fn auto_verify_cached_candidate(&mut self, track: &str) -> Result<bool, DbError> {
        if !self.mappings_for(track)?.is_empty() {
            return Ok(false);
        }
        let Some(recording) = self.canonical_track(track)? else {
            return Ok(false);
        };
        let mut candidates = self.resolution_candidates(track)?;
        crate::import::matching::rank(&recording.metadata, &mut candidates);
        let Some(candidate) =
            crate::import::matching::automatic_candidate(&recording.metadata, &candidates)
        else {
            return Ok(false);
        };
        let provider = candidate.provider;
        let external_id = candidate.external_id.clone();
        self.set_resolution(track, "needs_review", &candidates, None)?;
        self.verify_candidate(track, provider, &external_id)?;
        Ok(true)
    }
    pub fn verify_candidate(
        &mut self,
        track: &str,
        provider: Platform,
        external_id: &str,
    ) -> Result<(), DbError> {
        if provider != Platform::Youtube || !crate::import::urls::video_id(external_id) {
            return Err(invalid(
                "Only a valid YouTube candidate can be selected here.",
            ));
        }
        let candidate = self
            .resolution_candidates(track)?
            .into_iter()
            .find(|c| c.provider == provider && c.external_id == external_id)
            .ok_or_else(|| invalid("Search for this recording before selecting it."))?;
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO playback_mappings(track_id,provider,external_id,confidence,verified) VALUES(?1,?2,?3,?4,1) ON CONFLICT(track_id,provider) DO UPDATE SET external_id=excluded.external_id,confidence=excluded.confidence,verified=1,matched_at=CURRENT_TIMESTAMP",params![track,provider.as_str(),external_id,candidate.confidence])?;
        tx.execute(
            "UPDATE track_resolution SET state='matched',error=NULL WHERE track_id=?1",
            [track],
        )?;
        tx.commit()?;
        self.refresh_track_jobs(track)
    }
    pub fn invalidate_mapping(&mut self, track: &str, provider: Platform) -> Result<(), DbError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE playback_mappings SET verified=0 WHERE track_id=?1 AND provider=?2",
            params![track, provider.as_str()],
        )?;
        tx.execute(
            "UPDATE track_resolution SET state='pending',error=NULL WHERE track_id=?1",
            [track],
        )?;
        tx.commit()?;
        self.refresh_track_jobs(track)
    }
    pub fn mappings_for(&self, track: &str) -> Result<Vec<PlaybackMappingRow>, DbError> {
        let mut stmt=self.conn.prepare("SELECT track_id,provider,external_id,confidence,verified,matched_at FROM playback_mappings WHERE track_id=?1")?;
        let rows = stmt
            .query_map([track], |r| {
                Ok(PlaybackMappingRow {
                    track_id: r.get(0)?,
                    provider: r.get(1)?,
                    external_id: r.get(2)?,
                    confidence: r.get(3)?,
                    verified: r.get(4)?,
                    matched_at: r.get(5)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }
    fn refresh_track_jobs(&self, track: &str) -> Result<(), DbError> {
        let mut stmt=self.conn.prepare("SELECT DISTINCT j.id FROM import_jobs j JOIN playlist_tracks p ON p.playlist_id=j.playlist_id WHERE p.song_id=?1")?;
        let ids = stmt
            .query_map([track], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for id in ids {
            self.update_import_progress(&id)?;
            self.conn.execute("UPDATE import_jobs SET status='completed',error=NULL WHERE id=?1 AND status='error' AND playlist_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM playlist_tracks p JOIN track_resolution r ON r.track_id=p.song_id WHERE p.playlist_id=import_jobs.playlist_id AND r.state IN ('error','pending','searching'))",[id])?;
        }
        Ok(())
    }
    pub fn update_import_progress(&self, job: &str) -> Result<(), DbError> {
        self.conn.execute("UPDATE import_jobs SET processed=(SELECT COUNT(*) FROM playlist_tracks p JOIN track_resolution r ON r.track_id=p.song_id WHERE p.playlist_id=import_jobs.playlist_id AND r.state NOT IN ('pending','searching')),matched=(SELECT COUNT(*) FROM playlist_tracks p JOIN track_resolution r ON r.track_id=p.song_id WHERE p.playlist_id=import_jobs.playlist_id AND r.state='matched'),needs_review=(SELECT COUNT(*) FROM playlist_tracks p JOIN track_resolution r ON r.track_id=p.song_id WHERE p.playlist_id=import_jobs.playlist_id AND r.state='needs_review'),unavailable=skipped_tracks+(SELECT COUNT(*) FROM playlist_tracks p JOIN track_resolution r ON r.track_id=p.song_id WHERE p.playlist_id=import_jobs.playlist_id AND r.state='unavailable'),updated_at=CURRENT_TIMESTAMP WHERE id=?1",[job])?;
        Ok(())
    }
    pub fn finish_matching(&self, job: &str) -> Result<(), DbError> {
        self.update_import_progress(job)?;
        self.conn.execute("UPDATE import_jobs SET status=CASE WHEN EXISTS(SELECT 1 FROM playlist_tracks p JOIN track_resolution r ON r.track_id=p.song_id WHERE p.playlist_id=import_jobs.playlist_id AND r.state='error') THEN 'error' ELSE 'completed' END,error=(SELECT r.error FROM playlist_tracks p JOIN track_resolution r ON r.track_id=p.song_id WHERE p.playlist_id=import_jobs.playlist_id AND r.state='error' LIMIT 1),updated_at=CURRENT_TIMESTAMP WHERE id=?1 AND status='resolving'",[job])?;
        Ok(())
    }
    pub fn resume_import_job(&self, job: &str) -> Result<bool, DbError> {
        let has_playlist = self.import_job_playlist(job)?.is_some();
        self.conn.execute("UPDATE import_jobs SET status=?2,error=NULL,updated_at=CURRENT_TIMESTAMP WHERE id=?1 AND status IN ('interrupted','cancelled','error','completed')",params![job,if has_playlist {"resolving"} else {"fetching"}])?;
        Ok(has_playlist)
    }
}

fn validate_metadata(track: &ImportedTrack) -> Result<(), DbError> {
    let m = &track.metadata;
    if m.title.trim().is_empty()
        || m.title.len() > 4000
        || m.artists.len() > 100
        || m.artists.iter().any(|a| a.len() > 2000)
        || track.origin.external_id.is_empty()
        || track.origin.external_id.len() > 512
        || m.duration_ms.is_some_and(|d| d > 7 * 24 * 60 * 60 * 1000)
    {
        return Err(invalid("A track contains invalid or oversized metadata."));
    }
    Ok(())
}

fn read_track(conn: &Connection, id: &str) -> Result<Option<CapsuleTrack>, DbError> {
    let metadata = conn.query_row("SELECT title,artists,album,duration_ms,artwork_url,isrc,explicit FROM tracks WHERE id=?1",[id],|r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
    let Some((title, artists, album, duration_ms, artwork_url, isrc, explicit)) = metadata else {
        return Ok(None);
    };
    let mut stmt=conn.prepare("SELECT platform,external_id,url FROM track_origins WHERE track_id=?1 ORDER BY platform,external_id")?;
    let records = stmt
        .query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let origins = records
        .into_iter()
        .map(|(platform, external_id, url)| {
            Ok(Origin {
                platform: decode(&json(&platform)?)?,
                external_id,
                url,
            })
        })
        .collect::<Result<_, DbError>>()?;
    Ok(Some(CapsuleTrack {
        id: id.into(),
        metadata: Metadata {
            title,
            artists: decode(&artists)?,
            album,
            duration_ms,
            artwork_url,
            isrc,
            explicit,
        },
        origins,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{
        model::fixture,
        urls::{detect, ContentKind},
    };
    fn collection() -> ImportedCollection {
        ImportedCollection {
            source_url: "https://open.spotify.com/playlist/abc".into(),
            provider: Platform::Spotify,
            kind: ContentKind::Playlist,
            title: "My playlist".into(),
            artwork_url: None,
            tracks: vec![
                fixture("one", "Song"),
                fixture("two", "Song"),
                fixture("one", "Song"),
            ],
            warnings: vec![],
        }
    }
    fn ready(db: &mut Db, collection: &ImportedCollection) -> String {
        let job = db
            .create_import_job(&detect(&collection.source_url).unwrap())
            .unwrap();
        db.set_import_preview(&job.id, collection).unwrap();
        job.id
    }
    #[test]
    fn canonical_entries_preserve_order_duplicates_and_independent_uuids() {
        let mut db = Db::open_in_memory().unwrap();
        let c = collection();
        let job = ready(&mut db, &c);
        let playlist = db.confirm_import(&job).unwrap();
        let entries = db.import_entries(&playlist).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].track.id, entries[2].track.id);
        assert_ne!(
            entries[0].track.id, entries[1].track.id,
            "equal titles must not merge recordings"
        );
        assert_ne!(entries[0].entry_id, entries[2].entry_id);
        assert!(Uuid::parse_str(&entries[0].track.id).is_ok());
        assert!(Uuid::parse_str(&entries[0].entry_id).is_ok());
        assert_eq!(entries[0].track.metadata.title, "Song");
        assert_eq!(
            db.confirm_import(&job).unwrap(),
            playlist,
            "confirmation must be idempotent"
        );
    }
    #[test]
    fn canonical_library_survives_reopen_and_local_edits_do_not_refetch() {
        let path = std::env::temp_dir().join(format!("capsule-import-{}.sqlite3", Uuid::new_v4()));
        let (playlist, ids) = {
            let mut db = Db::open_at(&path).unwrap();
            let job = ready(&mut db, &collection());
            let playlist = db.confirm_import(&job).unwrap();
            let ids: Vec<_> = db
                .import_entries(&playlist)
                .unwrap()
                .iter()
                .map(|e| e.entry_id.clone())
                .collect();
            db.rename_import_playlist(&playlist, "Renamed").unwrap();
            db.reorder_import_playlist(
                &playlist,
                &[ids[2].clone(), ids[0].clone(), ids[1].clone()],
            )
            .unwrap();
            (playlist, ids)
        };
        {
            let db = Db::open_at(&path).unwrap();
            assert_eq!(db.playlists().unwrap()[0].name, "Renamed");
            let entries = db.import_entries(&playlist).unwrap();
            assert_eq!(entries[0].entry_id, ids[2]);
            assert_eq!(entries[1].entry_id, ids[0]);
        }
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn cancellation_and_restart_never_publish_partial_playlists() {
        let mut db = Db::open_in_memory().unwrap();
        let first = db
            .create_import_job(&detect(&collection().source_url).unwrap())
            .unwrap();
        db.cancel_import(&first.id).unwrap();
        assert!(db.set_import_preview(&first.id, &collection()).is_err());
        assert!(db.confirm_import(&first.id).is_err());
        let second = db
            .create_import_job(&detect(&collection().source_url).unwrap())
            .unwrap();
        db.recover_imports().unwrap();
        assert_eq!(
            db.get_import_job(&second.id).unwrap().unwrap().status,
            "interrupted"
        );
        assert!(db.playlists().unwrap().is_empty());
    }
    #[test]
    fn isrc_does_not_merge_incompatible_editions_and_original_origins_survive() {
        let mut db = Db::open_in_memory().unwrap();
        let mut c = collection();
        c.tracks.truncate(1);
        c.tracks[0].metadata.isrc = Some("US-AAA-12-34567".into());
        let job = ready(&mut db, &c);
        let p = db.confirm_import(&job).unwrap();
        let original = db.import_entries(&p).unwrap()[0].track.id.clone();
        c.tracks[0].origin.platform = Platform::AppleMusic;
        c.tracks[0].origin.external_id = "apple1".into();
        c.tracks[0].metadata.title = "Song (Live)".into();
        c.tracks[0].metadata.isrc = Some("USAAA1234567".into());
        let job = ready(&mut db, &c);
        let p2 = db.confirm_import(&job).unwrap();
        assert_ne!(db.import_entries(&p2).unwrap()[0].track.id, original);
        c.tracks[0].origin.external_id = "apple2".into();
        c.tracks[0].metadata.title = "Song".into();
        let job = ready(&mut db, &c);
        let p3 = db.confirm_import(&job).unwrap();
        assert_eq!(db.import_entries(&p3).unwrap()[0].track.id, original);
        assert_eq!(
            db.canonical_track(&original)
                .unwrap()
                .unwrap()
                .origins
                .len(),
            2
        );
    }
    #[test]
    fn invalid_reordering_is_atomic_and_cannot_cross_playlist_boundaries() {
        let mut db = Db::open_in_memory().unwrap();
        let job = ready(&mut db, &collection());
        let p = db.confirm_import(&job).unwrap();
        let before = db.import_entries(&p).unwrap();
        assert!(db
            .reorder_import_playlist(
                &p,
                &[
                    before[0].entry_id.clone(),
                    before[0].entry_id.clone(),
                    before[1].entry_id.clone()
                ]
            )
            .is_err());
        assert_eq!(db.import_entries(&p).unwrap(), before);
    }
    #[test]
    fn cached_match_refreshes_old_scores_and_preserves_existing_choices() {
        let mut db = Db::open_in_memory().unwrap();
        let job = ready(&mut db, &collection());
        let playlist = db.confirm_import(&job).unwrap();
        let track = db.import_entries(&playlist).unwrap()[0].track.clone();
        let mut candidate = PlaybackCandidate {
            provider: Platform::Youtube,
            external_id: "fn0EIOnK-D0".into(),
            metadata: track.metadata.clone(),
            confidence: 0.0,
            compatible: false,
        };
        db.set_resolution(&track.id, "needs_review", &[candidate.clone()], None)
            .unwrap();
        assert!(db.auto_verify_cached_candidate(&track.id).unwrap());
        let refreshed = db.resolution_candidates(&track.id).unwrap();
        assert!(refreshed[0].compatible);
        assert!(refreshed[0].confidence > 0.99);
        assert_eq!(
            db.resolution_state(&track.id).unwrap().as_deref(),
            Some("matched")
        );
        assert_eq!(
            db.mappings_for(&track.id).unwrap()[0].external_id,
            "fn0EIOnK-D0"
        );
        candidate.external_id = "O8CKxlmkZtw".into();
        db.set_resolution(&track.id, "needs_review", &[candidate.clone()], None)
            .unwrap();
        assert!(!db.auto_verify_cached_candidate(&track.id).unwrap());
        assert_eq!(
            db.mappings_for(&track.id).unwrap()[0].external_id,
            "fn0EIOnK-D0"
        );
        db.invalidate_mapping(&track.id, Platform::Youtube).unwrap();
        assert!(!db.auto_verify_cached_candidate(&track.id).unwrap());
        assert!(!db.mappings_for(&track.id).unwrap()[0].verified);
    }
    #[test]
    fn manual_mapping_and_cache_survive_retry_and_invalidation() {
        let mut db = Db::open_in_memory().unwrap();
        let job = ready(&mut db, &collection());
        let p = db.confirm_import(&job).unwrap();
        let track = db.import_entries(&p).unwrap()[0].track.clone();
        let candidate = PlaybackCandidate {
            provider: Platform::Youtube,
            external_id: "abcdefghijk".into(),
            metadata: track.metadata.clone(),
            confidence: 0.92,
            compatible: true,
        };
        assert!(db
            .verify_candidate(&track.id, Platform::Youtube, "abcdefghijk")
            .is_err());
        db.cache_candidates("Artist Song", std::slice::from_ref(&candidate))
            .unwrap();
        assert_eq!(
            db.cached_candidates("Artist Song").unwrap().unwrap().len(),
            1
        );
        db.set_resolution(
            &track.id,
            "needs_review",
            std::slice::from_ref(&candidate),
            None,
        )
        .unwrap();
        db.verify_candidate(&track.id, Platform::Youtube, "abcdefghijk")
            .unwrap();
        assert!(db.mappings_for(&track.id).unwrap()[0].verified);
        assert_eq!(
            db.get_import_job(&job).unwrap().unwrap().matched,
            2,
            "manual decisions update duplicate progress immediately"
        );
        db.invalidate_mapping(&track.id, Platform::Youtube).unwrap();
        assert!(!db.mappings_for(&track.id).unwrap()[0].verified);
        assert_eq!(
            db.resolution_state(&track.id).unwrap().as_deref(),
            Some("pending")
        );
    }
    #[test]
    fn reimport_does_not_resurrect_invalidated_youtube_mapping() {
        let mut db = Db::open_in_memory().unwrap();
        let mut c = collection();
        c.tracks.truncate(1);
        c.tracks[0].origin.platform = Platform::Youtube;
        c.tracks[0].origin.external_id = "abcdefghijk".into();
        let job = ready(&mut db, &c);
        let p = db.confirm_import(&job).unwrap();
        let track = db.import_entries(&p).unwrap()[0].track.id.clone();
        db.invalidate_mapping(&track, Platform::Youtube).unwrap();
        let job = ready(&mut db, &c);
        db.confirm_import(&job).unwrap();
        assert!(!db.mappings_for(&track).unwrap()[0].verified);
        assert_eq!(
            db.resolution_state(&track).unwrap().as_deref(),
            Some("pending")
        );
    }
    #[test]
    fn progress_reports_unavailable_and_retryable_errors_without_losing_playlist() {
        let mut db = Db::open_in_memory().unwrap();
        let job = ready(&mut db, &collection());
        let p = db.confirm_import(&job).unwrap();
        let entries = db.import_entries(&p).unwrap();
        db.set_resolution(&entries[0].track.id, "unavailable", &[], None)
            .unwrap();
        db.set_resolution(
            &entries[1].track.id,
            "error",
            &[],
            Some("Metadata API credentials expired."),
        )
        .unwrap();
        db.finish_matching(&job).unwrap();
        let info = db.get_import_job(&job).unwrap().unwrap();
        assert_eq!(info.status, "error");
        assert_eq!(info.processed, 3);
        assert_eq!(info.unavailable, 2);
        assert!(info.error.is_some());
        assert_eq!(db.import_entries(&p).unwrap().len(), 3);
    }
}
