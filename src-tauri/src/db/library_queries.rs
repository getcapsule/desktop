use super::{map_song, AlbumRow, ArtistRow, Db, DbError};
use rusqlite::params;

fn order_direction(direction: &str) -> Result<&'static str, DbError> {
    match direction {
        "asc" => Ok("ASC"),
        "desc" => Ok("DESC"),
        _ => Err(DbError::Import(
            "Unsupported library sort direction.".into(),
        )),
    }
}

impl Db {
    pub fn prepare_spotify_account(&mut self, identity: &str) -> Result<(), DbError> {
        self.prepare_account("spotify_account", "Spotify", identity)
    }

    pub fn prepare_youtube_account(&mut self, identity: &str) -> Result<(), DbError> {
        self.prepare_account("youtube_account", "YouTube Music", identity)
    }

    fn prepare_account(
        &mut self,
        key: &str,
        provider: &str,
        identity: &str,
    ) -> Result<(), DbError> {
        match self.meta(key)? {
            None => self.set_meta(key, identity),
            Some(saved) if saved == identity => Ok(()),
            Some(_) => Err(DbError::Import(format!(
                "The cached {provider} library belongs to a different account. An explicit account cache reset is required before connecting this account."
            ))),
        }
    }

    pub fn songs_query(
        &self,
        limit: u32,
        offset: u32,
        sort: &str,
        direction: &str,
        query: Option<&str>,
        artist_name: Option<&str>,
    ) -> Result<Vec<super::SongRow>, DbError> {
        // Only these constant identifiers and direction keywords enter SQL.
        let column = match sort {
            "title" => "name COLLATE NOCASE",
            "artist" => "artist_name COLLATE NOCASE",
            "album" => "album_name COLLATE NOCASE",
            "duration" => "duration_ms",
            _ => return Err(DbError::Import("Unsupported song sort.".into())),
        };
        let direction = order_direction(direction)?;
        let pattern = query.filter(|value| !value.trim().is_empty()).map(|value| {
            let escaped = value
                .trim()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            format!("%{escaped}%")
        });
        let sql = format!(
            "SELECT id, catalog_id, name, artist_name, album_name, duration_ms, artwork_url
             FROM songs
             WHERE (?1 IS NULL OR name LIKE ?1 ESCAPE '\\'
                    OR artist_name LIKE ?1 ESCAPE '\\'
                    OR album_name LIKE ?1 ESCAPE '\\')
               AND (?2 IS NULL OR artist_name = ?2)
             ORDER BY {column} {direction}, name COLLATE NOCASE ASC, id ASC
             LIMIT ?3 OFFSET ?4"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement
            .query_map(params![pattern, artist_name, limit, offset], map_song)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn albums_query(
        &self,
        limit: u32,
        offset: u32,
        sort: &str,
        direction: &str,
        artist_name: Option<&str>,
    ) -> Result<Vec<AlbumRow>, DbError> {
        let column = match sort {
            "title" => "name COLLATE NOCASE",
            "artist" => "artist_name COLLATE NOCASE",
            _ => return Err(DbError::Import("Unsupported album sort.".into())),
        };
        let direction = order_direction(direction)?;
        let sql = format!(
            "SELECT id, name, artist_name, artwork_url, track_count FROM albums
             WHERE (?1 IS NULL OR artist_name = ?1)
             ORDER BY {column} {direction}, name COLLATE NOCASE ASC, id ASC
             LIMIT ?2 OFFSET ?3"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement
            .query_map(params![artist_name, limit, offset], |row| {
                Ok(AlbumRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    artist_name: row.get(2)?,
                    artwork_url: row.get(3)?,
                    track_count: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn artists(&self) -> Result<Vec<ArtistRow>, DbError> {
        // Metadata credits are exact strings. A collaboration is one credit,
        // never split heuristically into artist accounts.
        let mut statement = self.conn.prepare(
            "WITH credits AS (
                SELECT name FROM artists WHERE trim(name) <> ''
                UNION SELECT artist_name FROM songs WHERE trim(artist_name) <> ''
                UNION SELECT artist_name FROM albums WHERE trim(artist_name) <> ''
             ), saved AS (
                SELECT name, MIN(id) AS id, MAX(artwork_url) AS artwork_url
                FROM artists GROUP BY name
             ), song_counts AS (
                SELECT artist_name AS name, COUNT(*) AS amount FROM songs GROUP BY artist_name
             ), album_counts AS (
                SELECT artist_name AS name, COUNT(*) AS amount FROM albums GROUP BY artist_name
             )
             SELECT COALESCE(saved.id, 'credit:' || credits.name), credits.name,
                    saved.artwork_url, COALESCE(song_counts.amount, 0), COALESCE(album_counts.amount, 0)
             FROM credits
             LEFT JOIN saved ON saved.name = credits.name
             LEFT JOIN song_counts ON song_counts.name = credits.name
             LEFT JOIN album_counts ON album_counts.name = credits.name
             ORDER BY credits.name COLLATE NOCASE, credits.name"
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(ArtistRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    artwork_url: row.get(2)?,
                    song_count: row.get(3)?,
                    album_count: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Db {
        let db = Db::open_in_memory().unwrap();
        for (id, title, artist, album, duration) in [
            ("a", "Alpha", "A & B", "Zulu", 30),
            ("b", "beta", "A", "Amber", 10),
            ("c", "beta", "A & B", "Amber", 20),
            ("d", "100%_\\literal", "O'Brien", "Other", 40),
        ] {
            db.conn.execute(
                "INSERT INTO songs (id,name,artist_name,album_name,duration_ms) VALUES (?1,?2,?3,?4,?5)",
                params![id, title, artist, album, duration],
            ).unwrap();
        }
        for (id, name, artist) in [
            ("al1", "Zulu", "A & B"),
            ("al2", "Amber", "A"),
            ("al3", "Other", "Album only"),
        ] {
            db.conn
                .execute(
                    "INSERT INTO albums (id,name,artist_name) VALUES (?1,?2,?3)",
                    params![id, name, artist],
                )
                .unwrap();
        }
        db.conn
            .execute(
                "INSERT INTO artists (id,name,artwork_url) VALUES ('ar1','Saved only','art')",
                [],
            )
            .unwrap();
        db
    }

    fn ids(rows: Vec<super::super::SongRow>) -> Vec<String> {
        rows.into_iter().map(|row| row.id).collect()
    }

    #[test]
    fn song_sorting_pages_have_stable_ties_and_numeric_duration() {
        let db = fixture();
        assert_eq!(
            ids(db
                .songs_query(10, 0, "duration", "asc", None, None)
                .unwrap()),
            ["b", "c", "a", "d"]
        );
        assert_eq!(
            ids(db.songs_query(2, 1, "title", "desc", None, None).unwrap()),
            ["c", "a"]
        );
        assert_eq!(
            ids(db.songs_query(10, 0, "artist", "asc", None, None).unwrap()),
            ["b", "a", "c", "d"]
        );
        assert_eq!(
            ids(db.songs_query(10, 0, "album", "asc", None, None).unwrap()),
            ["b", "c", "d", "a"]
        );
    }

    #[test]
    fn song_filters_match_literal_search_and_exact_artist_credits() {
        let db = fixture();
        assert_eq!(
            ids(db
                .songs_query(10, 0, "title", "asc", None, Some("A & B"))
                .unwrap()),
            ["a", "c"]
        );
        assert_eq!(
            ids(db
                .songs_query(10, 0, "title", "asc", Some("Amber"), Some("A & B"))
                .unwrap()),
            ["c"]
        );
        assert_eq!(
            ids(db
                .songs_query(10, 0, "title", "asc", Some("%_\\"), None)
                .unwrap()),
            ["d"]
        );
        assert_eq!(
            ids(db
                .songs_query(10, 0, "title", "asc", Some("O'Brien"), None)
                .unwrap()),
            ["d"]
        );
        assert!(db
            .songs_query(10, 0, "title", "asc", Some("' OR 1=1 --"), None)
            .unwrap()
            .is_empty());
        assert!(db
            .songs_query(10, 0, "title", "asc", None, Some("' OR 1=1 --"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn album_queries_sort_page_and_filter_bound_values() {
        let db = fixture();
        let rows = db.albums_query(1, 1, "title", "asc", None).unwrap();
        assert_eq!(rows[0].id, "al3");
        assert_eq!(
            db.albums_query(10, 0, "artist", "desc", None).unwrap()[0].id,
            "al3"
        );
        let rows = db
            .albums_query(10, 0, "title", "asc", Some("A & B"))
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "al1");
        assert!(db
            .albums_query(10, 0, "title", "asc", Some("' OR 1=1 --"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn sql_sort_inputs_are_closed_and_do_not_execute_sql() {
        let db = fixture();
        for (sort, direction) in [
            ("name; DROP TABLE songs; --", "asc"),
            ("title", "desc; DROP TABLE songs; --"),
        ] {
            assert!(db.songs_query(10, 0, sort, direction, None, None).is_err());
            assert!(db.albums_query(10, 0, sort, direction, None).is_err());
        }
        assert_eq!(db.songs(10, 0).unwrap().len(), 4);
    }

    #[test]
    fn artists_include_saved_and_cached_exact_credits_without_splitting_collaborations() {
        let db = fixture();
        let rows = db.artists().unwrap();
        assert_eq!(
            rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(),
            ["A", "A & B", "Album only", "O'Brien", "Saved only"]
        );
        let collaboration = rows.iter().find(|row| row.name == "A & B").unwrap();
        assert_eq!(
            (collaboration.song_count, collaboration.album_count),
            (2, 1)
        );
        let saved = rows.iter().find(|row| row.name == "Saved only").unwrap();
        assert_eq!((saved.song_count, saved.album_count), (0, 0));
        assert_eq!(saved.artwork_url.as_deref(), Some("art"));
        assert_eq!(saved.id, "ar1");
        assert!(!rows.iter().any(|row| row.name == "B"));
    }

    #[test]
    fn account_mismatch_preserves_all_cached_data_and_the_original_identity() {
        for (key, prepare) in [
            (
                "spotify_account",
                Db::prepare_spotify_account as fn(&mut Db, &str) -> Result<(), DbError>,
            ),
            (
                "youtube_account",
                Db::prepare_youtube_account as fn(&mut Db, &str) -> Result<(), DbError>,
            ),
        ] {
            let mut db = fixture();
            db.conn
                .execute(
                    "INSERT INTO playlists (id,name) VALUES ('p1','Favorites')",
                    [],
                )
                .unwrap();
            db.conn.execute("INSERT INTO playlist_tracks (playlist_id,position,song_id,entry_id) VALUES ('p1',0,'a','entry1')", []).unwrap();
            db.save_lyrics("a", None, Some("Words")).unwrap();
            db.set_meta("other_setting", "keep").unwrap();
            prepare(&mut db, "original").unwrap();
            prepare(&mut db, "original").unwrap();
            let error = prepare(&mut db, "different").unwrap_err();
            assert!(matches!(error, DbError::Import(_)));
            assert!(error.to_string().contains("explicit account cache reset"));
            assert_eq!(db.meta(key).unwrap().as_deref(), Some("original"));
            assert_eq!(db.meta("other_setting").unwrap().as_deref(), Some("keep"));
            assert_eq!(db.songs(10, 0).unwrap().len(), 4);
            assert_eq!(db.albums(10, 0).unwrap().len(), 3);
            assert_eq!(db.playlist_songs("p1").unwrap()[0].id, "a");
            assert_eq!(db.lyrics("a").unwrap().unwrap().1.as_deref(), Some("Words"));
            assert!(db.artists().unwrap().iter().any(|row| row.id == "ar1"));
        }
    }
}
