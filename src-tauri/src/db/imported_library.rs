//! A revision-cached SQL projection of provider metadata and canonical imports.
//! The persisted imports remain authoritative; no backfill or migration is needed.
use super::*;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex, OnceLock};

pub struct LibraryView {
    pub db: Db,
}

type CacheKey = (usize, u64, usize, u64, u64, String);
type CachedView = Option<(CacheKey, Arc<Mutex<LibraryView>>)>;
static CACHE: OnceLock<Mutex<CachedView>> = OnceLock::new();

impl Db {
    /// Temporary triggers track only metadata that affects this projection.
    /// Matching progress, query caches and playback preferences do not rebuild it.
    fn library_revision(&self) -> Result<u64, DbError> {
        let installed: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_temp_master WHERE name='library_revision')",
            [],
            |r| r.get(0),
        )?;
        if !installed {
            self.conn.execute_batch("CREATE TEMP TABLE library_revision (revision INTEGER NOT NULL); INSERT INTO library_revision VALUES(0);")?;
            for table in [
                "songs",
                "albums",
                "artists",
                "playlists",
                "tracks",
                "track_origins",
                "playback_mappings",
            ] {
                for operation in ["INSERT", "UPDATE", "DELETE"] {
                    self.conn.execute_batch(&format!("CREATE TEMP TRIGGER library_revision_{table}_{operation} AFTER {operation} ON main.{table} BEGIN UPDATE library_revision SET revision=revision+1; END;"))?;
                }
            }
        }
        let revision: u64 =
            self.conn
                .query_row("SELECT revision FROM library_revision", [], |r| r.get(0))?;
        let external: u64 = self
            .conn
            .pragma_query_value(None, "data_version", |r| r.get(0))?;
        Ok(revision.wrapping_add(external.rotate_left(32)))
    }
    pub fn has_imported_tracks(&self) -> Result<bool, DbError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tracks WHERE in_library=1)",
            [],
            |row| row.get(0),
        )?)
    }
}

impl LibraryView {
    pub fn cached(
        provider: &Db,
        imports: &Db,
        platform: &str,
        generation: u64,
    ) -> Result<Arc<Mutex<Self>>, DbError> {
        let key = (
            provider as *const Db as usize,
            provider.library_revision()?,
            imports as *const Db as usize,
            imports.library_revision()?,
            generation,
            platform.to_owned(),
        );
        let mut cache = CACHE
            .get_or_init(|| Mutex::new(None))
            .lock()
            .map_err(|_| DbError::Import("Library cache unavailable.".into()))?;
        if let Some((saved, view)) = &*cache {
            if saved == &key {
                return Ok(view.clone());
            }
        }
        let view = Arc::new(Mutex::new(Self::load(provider, Some(imports), platform)?));
        *cache = Some((key, view.clone()));
        Ok(view)
    }

    pub fn load(provider: &Db, imports: Option<&Db>, platform: &str) -> Result<Self, DbError> {
        let mut db = Db::init(Connection::open_in_memory()?)?;
        let tx = db.conn.transaction()?;
        // Fixed table names, identical schemas, and bound values preserve every
        // provider field, including album association and disc/track ordering.
        for table in ["songs", "albums", "artists", "playlists"] {
            let mut select = provider.conn.prepare(&format!("SELECT * FROM {table}"))?;
            let columns = select.column_count();
            let placeholders = vec!["?"; columns].join(",");
            let mut insert = tx.prepare(&format!("INSERT INTO {table} VALUES ({placeholders})"))?;
            let mut rows = select.query([])?;
            while let Some(row) = rows.next()? {
                let values = (0..columns)
                    .map(|i| row.get::<_, rusqlite::types::Value>(i))
                    .collect::<Result<Vec<_>, _>>()?;
                insert.execute(rusqlite::params_from_iter(values))?;
            }
        }
        tx.commit()?;
        if let Some(imports) = imports {
            let mut statement = imports
                .conn
                .prepare("SELECT id FROM tracks WHERE in_library=1 ORDER BY created_at, id")?;
            let ids = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            for id in ids {
                let Some(track) = imports.canonical_track(&id)? else {
                    continue;
                };
                let credit = track.metadata.artists.join(", ");
                let album = track
                    .metadata
                    .album
                    .as_deref()
                    .filter(|name| !name.trim().is_empty());
                let album_id = if let Some(name) = album {
                    let existing: Option<String> = db.conn.query_row("SELECT id FROM albums WHERE name=?1 AND artist_name=?2 ORDER BY id LIMIT 1", params![name, credit], |r| r.get(0)).optional()?;
                    let album_id = existing.unwrap_or_else(|| imported_album_id(name, &credit));
                    db.conn.execute("INSERT INTO albums(id,name,artist_name,artwork_url) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET artwork_url=COALESCE(albums.artwork_url,excluded.artwork_url)", params![album_id, name, credit, track.metadata.artwork_url])?;
                    Some(album_id)
                } else {
                    None
                };
                let mut external_ids: Vec<String> = track
                    .origins
                    .iter()
                    .filter(|origin| origin.platform.as_str() == platform)
                    .map(|origin| origin.external_id.clone())
                    .collect();
                external_ids.extend(
                    imports
                        .mappings_for(&id)?
                        .into_iter()
                        .filter(|mapping| mapping.provider == platform && mapping.verified)
                        .map(|mapping| mapping.external_id),
                );
                let mut ordering = (None::<i64>, None::<i64>);
                for external in external_ids {
                    let existing: Option<(Option<i64>, Option<i64>)> = db.conn.query_row("SELECT track_number,disc_number FROM songs WHERE id=?1 OR catalog_id=?1 LIMIT 1", [&external], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
                    if let Some(numbers) = existing {
                        ordering = numbers;
                    }
                    db.conn.execute(
                        "DELETE FROM songs WHERE id=?1 OR catalog_id=?1",
                        [&external],
                    )?;
                }
                db.conn.execute("INSERT OR REPLACE INTO songs(id,catalog_id,name,artist_name,album_name,album_id,duration_ms,artwork_url,track_number,disc_number) VALUES(?1,?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![id, track.metadata.title, credit, album.unwrap_or_default(), album_id, track.metadata.duration_ms.unwrap_or(0), track.metadata.artwork_url, ordering.0, ordering.1])?;
            }
            for playlist in imports.playlists()? {
                db.conn.execute(
                    "INSERT OR IGNORE INTO playlists(id,name,artwork_url) VALUES(?1,?2,?3)",
                    params![playlist.id, playlist.name, playlist.artwork_url],
                )?;
            }
            db.conn.execute("UPDATE albums SET track_count=(SELECT COUNT(*) FROM songs WHERE album_id=albums.id) WHERE id LIKE 'import-album:%' OR EXISTS(SELECT 1 FROM songs WHERE album_id=albums.id AND id=catalog_id AND length(id)=36)", [])?;
        }
        db.projected_counts = Some(db.counts()?);
        Ok(Self { db })
    }

    #[cfg(test)]
    fn counts(&self) -> Result<LibraryCounts, DbError> {
        self.db.counts()
    }
}

fn imported_album_id(album: &str, credit: &str) -> String {
    // Length-delimited JSON prevents ambiguous artist/title concatenations.
    let digest =
        Sha256::digest(serde_json::to_vec(&(album, credit)).expect("string tuple serialization"));
    format!("import-album:{digest:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    // These tests share the process-wide one-entry projection cache.
    static CACHE_TEST_LOCK: Mutex<()> = Mutex::new(());

    use crate::import::{
        model::{fixture, ImportedCollection, Platform},
        urls::{detect, ContentKind},
    };

    fn import(db: &mut Db) -> String {
        let mut first = fixture("one", "Zulu");
        first.metadata.artists = vec!["A".into(), "B".into()];
        first.metadata.duration_ms = Some(30);
        let mut second = fixture("two", "Alpha");
        second.metadata.album = None;
        second.metadata.duration_ms = Some(10);
        let collection = ImportedCollection {
            source_url: "https://open.spotify.com/playlist/abc".into(),
            provider: Platform::Spotify,
            kind: ContentKind::Playlist,
            title: "Playlist is not an album".into(),
            artwork_url: None,
            tracks: vec![first.clone(), second, first],
            warnings: vec![],
        };
        let job = db
            .create_import_job(&detect(&collection.source_url).unwrap())
            .unwrap();
        db.set_import_preview(&job.id, &collection).unwrap();
        db.confirm_import(&job.id).unwrap()
    }

    #[test]
    fn explicit_import_promotes_cached_tracks_and_replay_does_not_hide_saved_songs() {
        let provider = Db::open_in_memory().unwrap();
        let mut imports = Db::open_in_memory().unwrap();
        let cached = imports.save_source_track(&fixture("one", "Zulu")).unwrap();
        let playlist = import(&mut imports);
        assert_eq!(
            imports.import_entries(&playlist).unwrap()[0].track.id,
            cached.id
        );
        imports.save_source_track(&fixture("one", "Zulu")).unwrap();
        let library = LibraryView::load(&provider, Some(&imports), "local").unwrap();
        assert_eq!(library.db.counts().unwrap().songs, 2);
        assert!(library.db.song_by_id(&cached.id).unwrap().is_some());
        assert!(imports.has_imported_tracks().unwrap());
    }

    #[test]
    fn library_membership_migration_preserves_previously_saved_tracks() {
        let conn = Connection::open_in_memory().unwrap();
        let previous = MIGRATIONS.len() - 1;
        for sql in MIGRATIONS.iter().take(previous) {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", previous).unwrap();
        conn.execute(
            "INSERT INTO tracks(id,title,artists) VALUES('saved','Saved song','[\"Artist\"]')",
            [],
        )
        .unwrap();
        let imports = Db::init(conn).unwrap();
        let library =
            LibraryView::load(&Db::open_in_memory().unwrap(), Some(&imports), "local").unwrap();
        assert!(library.db.song_by_id("saved").unwrap().is_some());
        assert!(imports.has_imported_tracks().unwrap());
    }

    #[test]
    fn offline_download_promotes_playback_only_track_and_invalidates_cached_library() {
        let _cache_test = CACHE_TEST_LOCK.lock().unwrap();
        let provider = Db::open_in_memory().unwrap();
        let mut imports = Db::open_in_memory().unwrap();
        let mut recording = fixture("42", "Downloaded song");
        recording.origin.platform = Platform::Soundcloud;
        let cached = imports.save_source_track(&recording).unwrap();
        let before = LibraryView::cached(&provider, &imports, "local", 1).unwrap();
        assert_eq!(before.lock().unwrap().db.counts().unwrap().songs, 0);
        imports
            .attach_playback_origin(
                &cached.id,
                &crate::import::model::Origin {
                    platform: Platform::Local,
                    external_id: "downloaded-song.flac".into(),
                    url: None,
                },
            )
            .unwrap();
        let after = LibraryView::cached(&provider, &imports, "local", 1).unwrap();
        assert!(!Arc::ptr_eq(&before, &after));
        assert_eq!(after.lock().unwrap().db.counts().unwrap().songs, 1);
        assert!(after
            .lock()
            .unwrap()
            .db
            .song_by_id(&cached.id)
            .unwrap()
            .is_some());
    }

    #[test]
    fn imported_album_reuses_matching_provider_album_and_preserves_track_order() {
        let provider = Db::open_in_memory().unwrap();
        provider.conn.execute("INSERT INTO albums(id,name,artist_name,track_count) VALUES('album','Album','A, B',2)", []).unwrap();
        provider.conn.execute("INSERT INTO songs(id,catalog_id,name,artist_name,album_name,album_id,track_number,disc_number) VALUES('one','one','Zulu','A, B','Album','album',2,1),('provider',NULL,'Beta','A, B','Album','album',1,1)", []).unwrap();
        let mut imports = Db::open_in_memory().unwrap();
        let playlist = import(&mut imports);
        let canonical_id = imports.import_entries(&playlist).unwrap()[0]
            .track
            .id
            .clone();
        imports
            .conn
            .execute(
                "UPDATE tracks SET artwork_url='https://i.scdn.co/image/album-cover' WHERE id=?1",
                [&canonical_id],
            )
            .unwrap();
        let view = LibraryView::load(&provider, Some(&imports), "spotify").unwrap();
        let albums = view.db.albums(100, 0).unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!((albums[0].id.as_str(), albums[0].track_count), ("album", 2));
        let songs = view.db.album_songs("album").unwrap();
        assert_eq!(
            songs
                .iter()
                .map(|song| song.id.as_str())
                .collect::<Vec<_>>(),
            ["provider", canonical_id.as_str()]
        );
        assert_eq!(
            view.db.artwork_for("album").unwrap().unwrap().template,
            "https://i.scdn.co/image/album-cover"
        );
    }
    #[test]
    fn merged_queries_sort_filter_page_group_and_count_unique_persisted_tracks() {
        let provider = Db::open_in_memory().unwrap();
        provider.conn.execute("INSERT INTO songs(id,name,artist_name,album_name,duration_ms) VALUES('provider','Beta','A, B','',20)", []).unwrap();
        let mut imports = Db::open_in_memory().unwrap();
        import(&mut imports);
        import(&mut imports);
        let view = LibraryView::load(&provider, Some(&imports), "local").unwrap();
        let page = view
            .db
            .songs_query(1, 1, "duration", "desc", None, None)
            .unwrap();
        assert_eq!(page[0].name, "Beta");
        let rows = view
            .db
            .songs_query(100, 0, "title", "asc", Some("Album"), Some("A, B"))
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Zulu");
        assert!(view
            .db
            .songs_query(100, 0, "title", "asc", Some("%_"), None)
            .unwrap()
            .is_empty());
        let albums = view.db.albums_query(100, 0, "title", "asc", None).unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].name, "Album");
        assert!(albums[0].id.starts_with("import-album:"));
        assert_eq!(view.db.album_songs(&albums[0].id).unwrap().len(), 1);
        assert_eq!(albums[0].track_count, 1);
        let artists = view.db.artists().unwrap();
        let collaboration = artists.iter().find(|row| row.name == "A, B").unwrap();
        assert_eq!(
            (collaboration.song_count, collaboration.album_count),
            (2, 1)
        );
        assert!(!artists.iter().any(|row| row.name == "B"));
        assert_eq!(view.db.search("Zulu", 100).unwrap().len(), 1);
        let counts = view.counts().unwrap();
        assert_eq!(
            (
                counts.songs,
                counts.albums,
                counts.artists,
                counts.playlists
            ),
            (3, 1, 2, 2)
        );
    }

    #[test]
    fn matching_provider_origin_is_one_song_and_other_provider_ids_do_not_collide() {
        let provider = Db::open_in_memory().unwrap();
        provider
            .conn
            .execute(
                "INSERT INTO songs(id,catalog_id,name) VALUES('one','one','Provider')",
                [],
            )
            .unwrap();
        let mut imports = Db::open_in_memory().unwrap();
        import(&mut imports);
        let merged = LibraryView::load(&provider, Some(&imports), "spotify").unwrap();
        assert_eq!(merged.counts().unwrap().songs, 2);
        assert!(!merged
            .db
            .songs(100, 0)
            .unwrap()
            .iter()
            .any(|row| row.id == "one"));
        let local = LibraryView::load(&provider, Some(&imports), "local").unwrap();
        assert_eq!(local.counts().unwrap().songs, 3);
    }

    #[test]
    fn cached_projection_reuses_pages_ignores_progress_and_invalidates_metadata() {
        let _cache_test = CACHE_TEST_LOCK.lock().unwrap();
        let provider = Db::open_in_memory().unwrap();
        let mut imports = Db::open_in_memory().unwrap();
        let playlist = import(&mut imports);
        let first = LibraryView::cached(&provider, &imports, "local", 1).unwrap();
        let second = LibraryView::cached(&provider, &imports, "local", 1).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        let track = imports.import_entries(&playlist).unwrap()[0]
            .track
            .id
            .clone();
        imports
            .set_resolution(&track, "searching", &[], None)
            .unwrap();
        let progress = LibraryView::cached(&provider, &imports, "local", 1).unwrap();
        assert!(Arc::ptr_eq(&first, &progress));
        imports
            .conn
            .execute("UPDATE tracks SET title='Updated' WHERE id=?1", [&track])
            .unwrap();
        let changed = LibraryView::cached(&provider, &imports, "local", 1).unwrap();
        assert!(!Arc::ptr_eq(&first, &changed));
        assert_eq!(
            changed
                .lock()
                .unwrap()
                .db
                .search("Updated", 10)
                .unwrap()
                .len(),
            1
        );
        assert!(!Arc::ptr_eq(
            &changed,
            &LibraryView::cached(&provider, &imports, "local", 2).unwrap()
        ));
    }

    #[test]
    fn persisted_existing_imports_appear_after_reopen_with_stable_album_ids() {
        let path = std::env::temp_dir().join(format!(
            "capsule-library-view-{}.sqlite3",
            uuid::Uuid::new_v4()
        ));
        let provider = Db::open_in_memory().unwrap();
        let album_id = {
            let mut imports = Db::open_at(&path).unwrap();
            import(&mut imports);
            LibraryView::load(&provider, Some(&imports), "local")
                .unwrap()
                .db
                .albums(100, 0)
                .unwrap()[0]
                .id
                .clone()
        };
        let imports = Db::open_at(&path).unwrap();
        let view = LibraryView::load(&provider, Some(&imports), "youtube").unwrap();
        assert_eq!(view.db.albums(100, 0).unwrap()[0].id, album_id);
        assert_eq!(view.db.album_songs(&album_id).unwrap().len(), 1);
        drop(imports);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn imported_tracks_are_available_in_song_queries_without_changing_playlist_occurrences() {
        let mut db = Db::open_in_memory().unwrap();
        let playlist = import(&mut db);
        assert_eq!(db.import_entries(&playlist).unwrap().len(), 3);
        assert_eq!(
            LibraryView::load(&Db::open_in_memory().unwrap(), Some(&db), "local")
                .unwrap()
                .db
                .songs_query(100, 0, "title", "asc", None, None)
                .unwrap()
                .len(),
            2
        );
    }
}
