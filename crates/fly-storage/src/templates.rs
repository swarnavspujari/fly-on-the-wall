//! Note templates: per-meeting-type system prompt + structure hint.
//! Built-ins are seeded once and can be edited (they keep `built_in = 1`
//! purely as a "don't panic, you can reset" marker).

use fly_core::Template;

use crate::{Result, Storage, StorageError};

impl Storage {
    pub fn list_templates(&self) -> Result<Vec<Template>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, system_prompt, structure_hint, built_in FROM templates ORDER BY built_in DESC, name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Template {
                id: r.get(0)?,
                name: r.get(1)?,
                system_prompt: r.get(2)?,
                structure_hint: r.get(3)?,
                built_in: r.get::<_, i64>(4)? != 0,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn get_template(&self, id: &str) -> Result<Template> {
        self.list_templates()?
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| StorageError::NotFound(format!("template {id}")))
    }

    pub fn upsert_template(&self, t: &Template) -> Result<()> {
        self.conn.execute(
            "INSERT INTO templates (id, name, system_prompt, structure_hint, built_in)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name,
               system_prompt = excluded.system_prompt,
               structure_hint = excluded.structure_hint",
            (
                &t.id,
                &t.name,
                &t.system_prompt,
                &t.structure_hint,
                i64::from(t.built_in),
            ),
        )?;
        Ok(())
    }

    pub fn delete_template(&self, id: &str) -> Result<()> {
        let n = self
            .conn
            .execute("DELETE FROM templates WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("template {id}")));
        }
        Ok(())
    }

    /// Seed the default templates once (spec §9: 1:1, Sales discovery,
    /// Standup, Interview, General).
    pub fn seed_builtin_templates(&self) -> Result<()> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM templates", [], |r| r.get(0))?;
        if count > 0 {
            return Ok(());
        }
        for (id, name, prompt, hint) in BUILTINS {
            self.upsert_template(&Template {
                id: (*id).into(),
                name: (*name).into(),
                system_prompt: (*prompt).into(),
                structure_hint: (*hint).into(),
                built_in: true,
            })?;
        }
        Ok(())
    }
}

const BUILTINS: &[(&str, &str, &str, &str)] = &[
    (
        "tpl-general",
        "General meeting",
        "You are an expert meeting-notes editor. Merge the user's rough notes with the transcript into clean, faithful notes. Never invent facts that are in neither source. Prefer the user's phrasing for things they wrote themselves.",
        "## Summary\n## Key points\n## Decisions\n## Action items (owner — due)",
    ),
    (
        "tpl-one-on-one",
        "1:1",
        "You are summarizing a 1:1 conversation. Be personal and concrete; capture feedback in the giver's own words, growth topics, and commitments from each side. Never invent facts.",
        "## Topics discussed\n## Feedback\n## Commitments\n## Follow-ups for next time",
    ),
    (
        "tpl-sales-discovery",
        "Sales discovery",
        "You are summarizing a sales discovery call. Extract the prospect's pain, current stack, budget signals, decision process, and objections — with who said what. Never invent facts.",
        "## Company & contact\n## Pain points\n## Current solution\n## Budget & timeline\n## Decision process\n## Objections\n## Next steps",
    ),
    (
        "tpl-standup",
        "Standup",
        "You are summarizing a team standup. Group by person: what they did, what they will do, and blockers. Keep it terse. Never invent facts.",
        "## Per person\n### <name>\n- Did\n- Next\n- Blockers\n## Cross-team blockers",
    ),
    (
        "tpl-interview",
        "Interview",
        "You are summarizing a candidate interview. Capture the candidate's experience, concrete examples they gave, strengths, concerns, and any follow-up questions — grounded strictly in the transcript. Never invent facts.",
        "## Candidate background\n## Strong signals\n## Concerns\n## Notable answers\n## Recommended follow-ups",
    ),
];

#[cfg(test)]
mod tests {
    use crate::test_storage;

    #[test]
    fn seeding_is_idempotent_and_editable() {
        let (_dir, s) = test_storage();
        s.seed_builtin_templates().unwrap();
        s.seed_builtin_templates().unwrap();
        let templates = s.list_templates().unwrap();
        assert_eq!(templates.len(), 5);

        let mut t = s.get_template("tpl-standup").unwrap();
        t.system_prompt = "custom".into();
        s.upsert_template(&t).unwrap();
        assert_eq!(
            s.get_template("tpl-standup").unwrap().system_prompt,
            "custom"
        );

        s.delete_template("tpl-interview").unwrap();
        assert_eq!(s.list_templates().unwrap().len(), 4);
    }
}
