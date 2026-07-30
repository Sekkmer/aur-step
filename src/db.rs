use crate::{config::Config, fs_safety, model::PackageRecord};
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

    pub fn delete_package(&self, package: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM packages WHERE aur_name = ?1", params![package])?;
        Ok(())
    }
}
