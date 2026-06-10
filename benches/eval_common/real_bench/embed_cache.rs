//! SQLite-backed embedding cache for bench runs. Keyed by (model, text).

use rusqlite::Connection;

use super::error::{BenchError, BenchResult};

pub struct EmbedCache {
    conn: Connection,
    model: String,
}

impl EmbedCache {
    pub fn open(path: &std::path::Path, model: &str) -> BenchResult<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| BenchError::InvalidInput(e.to_string()))?;
            }
        }
        let conn = Connection::open(path).map_err(|e| BenchError::Engine(e.to_string()))?;
        // WAL + relaxed sync: cold-cache population writes hundreds of
        // thousands of rows; per-row fsync under the default journal mode
        // would dominate the very cost this cache exists to remove.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS embeddings (
                model TEXT NOT NULL,
                text  TEXT NOT NULL,
                vec   BLOB NOT NULL,
                PRIMARY KEY (model, text)
            );",
        )
        .map_err(|e| BenchError::Engine(e.to_string()))?;
        Ok(Self { conn, model: model.to_string() })
    }

    pub fn get(&self, text: &str) -> BenchResult<Option<Vec<f64>>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT vec FROM embeddings WHERE model = ?1 AND text = ?2")
            .map_err(|e| BenchError::Engine(e.to_string()))?;
        let row: Option<Vec<u8>> = stmt
            .query_row((&self.model as &str, text), |row| row.get(0))
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(|e| BenchError::Engine(e.to_string()))?;
        Ok(row.map(|bytes| decode(&bytes)))
    }

    pub fn put(&self, text: &str, vec: &[f64]) -> BenchResult<()> {
        let mut stmt = self
            .conn
            .prepare_cached(
                "INSERT OR REPLACE INTO embeddings (model, text, vec) VALUES (?1, ?2, ?3)",
            )
            .map_err(|e| BenchError::Engine(e.to_string()))?;
        stmt.execute(rusqlite::params![&self.model, text, encode(vec)])
            .map_err(|e| BenchError::Engine(e.to_string()))?;
        Ok(())
    }
}

fn encode(vec: &[f64]) -> Vec<u8> {
    vec.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn decode(bytes: &[u8]) -> Vec<f64> {
    bytes
        .chunks_exact(8)
        .map(|chunk| f64::from_le_bytes(chunk.try_into().expect("8-byte chunk")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_model_isolation() {
        let dir = std::env::temp_dir().join(format!("embed-cache-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cache.sqlite");
        let cache = EmbedCache::open(&path, "model-a").unwrap();
        assert_eq!(cache.get("hello").unwrap(), None);
        cache.put("hello", &[0.25, -1.5]).unwrap();
        assert_eq!(cache.get("hello").unwrap(), Some(vec![0.25, -1.5]));
        // A different model must not see model-a's rows.
        let other = EmbedCache::open(&path, "model-b").unwrap();
        assert_eq!(other.get("hello").unwrap(), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
