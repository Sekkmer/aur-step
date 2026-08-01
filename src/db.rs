use crate::{
    config::Config,
    fs_safety,
    model::{ArtifactRecord, JournalRecord, PackageRecord, TrustRecord},
};
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(config: &Config) -> Result<Self> {
        fs_safety::prepare_state_database(&config.state_db)?;
        let conn = Connection::open(&config.state_db)
            .with_context(|| format!("failed to open {}", config.state_db))?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    pub fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS packages (
                aur_name TEXT PRIMARY KEY,
                repo_url TEXT NOT NULL,
                build_path TEXT NOT NULL,
                last_built_version TEXT,
                last_installed_version TEXT,
                last_commit TEXT,
                reviewed_commit TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS package_trust (
                aur_name TEXT PRIMARY KEY REFERENCES packages(aur_name) ON DELETE CASCADE,
                observed_maintainer TEXT,
                reviewed_maintainer TEXT,
                observed_at TEXT,
                reviewed_at TEXT,
                observed_sources_json TEXT NOT NULL DEFAULT '[]',
                reviewed_sources_json TEXT NOT NULL DEFAULT '[]'
            );

            CREATE TABLE IF NOT EXISTS build_artifacts (
                aur_name TEXT NOT NULL REFERENCES packages(aur_name) ON DELETE CASCADE,
                path TEXT NOT NULL,
                commit_sha TEXT NOT NULL,
                sha256 TEXT NOT NULL,
                manifest_sha256 TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (aur_name, path)
            );

            CREATE TABLE IF NOT EXISTS journal (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                action TEXT NOT NULL,
                aur_name TEXT,
                commit_sha TEXT,
                details_json TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    pub fn upsert_package_stub(&self, package: &str, build_path: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let repo_url = format!("https://aur.archlinux.org/{package}.git");
        self.conn.execute(
            r#"
            INSERT INTO packages (aur_name, repo_url, build_path, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?4)
            ON CONFLICT(aur_name) DO UPDATE SET
                repo_url = excluded.repo_url,
                build_path = excluded.build_path,
                updated_at = excluded.updated_at
            "#,
            params![package, repo_url, build_path, now],
        )?;
        Ok(())
    }

    pub fn upsert_imported_package(
        &self,
        package: &str,
        repo_url: &str,
        build_path: &str,
        installed_version: &str,
        last_commit: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            INSERT INTO packages (
                aur_name, repo_url, build_path, last_installed_version,
                last_commit, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
            ON CONFLICT(aur_name) DO UPDATE SET
                repo_url = excluded.repo_url,
                build_path = excluded.build_path,
                last_installed_version = excluded.last_installed_version,
                last_commit = excluded.last_commit,
                updated_at = excluded.updated_at
            "#,
            params![
                package,
                repo_url,
                build_path,
                installed_version,
                last_commit,
                now
            ],
        )?;
        Ok(())
    }

    pub fn list_packages(&self) -> Result<Vec<PackageRecord>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT aur_name, repo_url, build_path, last_built_version,
                   last_installed_version, last_commit, reviewed_commit
            FROM packages
            ORDER BY aur_name
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(PackageRecord {
                aur_name: row.get(0)?,
                repo_url: row.get(1)?,
                build_path: row.get(2)?,
                last_built_version: row.get(3)?,
                last_installed_version: row.get(4)?,
                last_commit: row.get(5)?,
                reviewed_commit: row.get(6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_package(&self, package: &str) -> Result<Option<PackageRecord>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT aur_name, repo_url, build_path, last_built_version,
                   last_installed_version, last_commit, reviewed_commit
            FROM packages
            WHERE aur_name = ?1
            "#,
        )?;
        let mut rows = stmt.query(params![package])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(PackageRecord {
            aur_name: row.get(0)?,
            repo_url: row.get(1)?,
            build_path: row.get(2)?,
            last_built_version: row.get(3)?,
            last_installed_version: row.get(4)?,
            last_commit: row.get(5)?,
            reviewed_commit: row.get(6)?,
        }))
    }

    pub fn update_last_commit(&self, package: &str, last_commit: Option<&str>) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            UPDATE packages
            SET last_commit = ?2, updated_at = ?3
            WHERE aur_name = ?1
            "#,
            params![package, last_commit, now],
        )?;
        Ok(())
    }

    pub fn update_last_built_version(&self, package: &str, version: Option<&str>) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            UPDATE packages
            SET last_built_version = ?2, updated_at = ?3
            WHERE aur_name = ?1
            "#,
            params![package, version, now],
        )?;
        Ok(())
    }

    pub fn update_last_installed_version(
        &self,
        package: &str,
        version: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            UPDATE packages
            SET last_installed_version = ?2, updated_at = ?3
            WHERE aur_name = ?1
            "#,
            params![package, version, now],
        )?;
        Ok(())
    }

    pub fn update_reviewed_commit(&self, package: &str, reviewed_commit: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            UPDATE packages
            SET reviewed_commit = ?2, updated_at = ?3
            WHERE aur_name = ?1
            "#,
            params![package, reviewed_commit, now],
        )?;
        Ok(())
    }

    pub fn update_observed_trust(
        &self,
        package: &str,
        maintainer: Option<&str>,
        sources: &[String],
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let sources = serde_json::to_string(sources)?;
        self.conn.execute(
            r#"
            INSERT INTO package_trust (
                aur_name, observed_maintainer, observed_at, observed_sources_json
            ) VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(aur_name) DO UPDATE SET
                observed_maintainer = excluded.observed_maintainer,
                observed_at = excluded.observed_at,
                observed_sources_json = excluded.observed_sources_json
            "#,
            params![package, maintainer, now, sources],
        )?;
        Ok(())
    }

    pub fn approve_observed_maintainer(&self, package: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            UPDATE package_trust
            SET reviewed_maintainer = observed_maintainer,
                reviewed_sources_json = observed_sources_json,
                reviewed_at = ?2
            WHERE aur_name = ?1
            "#,
            params![package, now],
        )?;
        Ok(())
    }

    pub fn get_trust(&self, package: &str) -> Result<Option<TrustRecord>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT observed_maintainer, reviewed_maintainer, observed_at, reviewed_at,
                   observed_sources_json, reviewed_sources_json
            FROM package_trust WHERE aur_name = ?1
            "#,
        )?;
        let mut rows = stmt.query(params![package])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let observed_sources: String = row.get(4)?;
        let reviewed_sources: String = row.get(5)?;
        Ok(Some(TrustRecord {
            observed_maintainer: row.get(0)?,
            reviewed_maintainer: row.get(1)?,
            observed_at: row.get(2)?,
            reviewed_at: row.get(3)?,
            observed_sources: serde_json::from_str(&observed_sources).unwrap_or_default(),
            reviewed_sources: serde_json::from_str(&reviewed_sources).unwrap_or_default(),
        }))
    }

    pub fn replace_build_artifacts(&self, package: &str, records: &[ArtifactRecord]) -> Result<()> {
        self.conn.execute(
            "DELETE FROM build_artifacts WHERE aur_name = ?1",
            params![package],
        )?;
        let now = Utc::now().to_rfc3339();
        for record in records {
            self.conn.execute(
                r#"
                INSERT INTO build_artifacts (
                    aur_name, path, commit_sha, sha256, manifest_sha256, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
                params![
                    package,
                    record.path,
                    record.commit,
                    record.sha256,
                    record.manifest_sha256,
                    now
                ],
            )?;
        }
        Ok(())
    }

    pub fn get_build_artifact(&self, package: &str, path: &str) -> Result<Option<ArtifactRecord>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT path, commit_sha, sha256, manifest_sha256
            FROM build_artifacts WHERE aur_name = ?1 AND path = ?2
            "#,
        )?;
        let mut rows = stmt.query(params![package, path])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(ArtifactRecord {
            path: row.get(0)?,
            commit: row.get(1)?,
            sha256: row.get(2)?,
            manifest_sha256: row.get(3)?,
        }))
    }

    pub fn list_build_artifacts(&self, package: &str) -> Result<Vec<ArtifactRecord>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT path, commit_sha, sha256, manifest_sha256
            FROM build_artifacts WHERE aur_name = ?1 ORDER BY path
            "#,
        )?;
        let rows = stmt.query_map(params![package], |row| {
            Ok(ArtifactRecord {
                path: row.get(0)?,
                commit: row.get(1)?,
                sha256: row.get(2)?,
                manifest_sha256: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_journal(&self, package: &str) -> Result<Vec<JournalRecord>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT action, commit_sha, details_json, created_at
            FROM journal WHERE aur_name = ?1 ORDER BY id
            "#,
        )?;
        let rows = stmt.query_map(params![package], |row| {
            let details: String = row.get(2)?;
            Ok((row.get(0)?, row.get(1)?, details, row.get(3)?))
        })?;
        rows.map(|row| {
            let (action, commit, details, created_at): (String, Option<String>, String, String) =
                row?;
            Ok(JournalRecord {
                action,
                commit,
                details: serde_json::from_str(&details)
                    .unwrap_or_else(|_| serde_json::Value::String(details)),
                created_at,
            })
        })
        .collect()
    }

    pub fn journal<T: serde::Serialize>(
        &self,
        action: &str,
        package: Option<&str>,
        commit: Option<&str>,
        details: &T,
    ) -> Result<()> {
        let details_json = serde_json::to_string(details)?;
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            INSERT INTO journal (action, aur_name, commit_sha, details_json, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![action, package, commit, details_json, now],
        )?;
        Ok(())
    }

    pub fn delete_package(&self, package: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM packages WHERE aur_name = ?1", params![package])?;
        Ok(())
    }
}
