//! File name patterns for exports (SPEC 12.2, "Customize...").

/// Values substituted into a pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct NameContext {
    pub connection: String,
    pub database: String,
    pub tag: String,
    /// `YYYYMMDD`.
    pub date: String,
    /// `HHMMSS`.
    pub time: String,
}

impl NameContext {
    /// Context with the current local date and time.
    pub fn now(connection: &str, database: &str, tag: &str) -> Self {
        let n = chrono::Local::now();
        Self { connection: connection.into(), database: database.into(), tag: tag.into(), date: n.format("%Y%m%d").to_string(), time: n.format("%H%M%S").to_string() }
    }
}

/// Characters not allowed in file names on common platforms.
const FORBIDDEN: [char; 9] = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// Replaces tokens (`{connection}`, `{database}`, `{table}`, `{tag}`, `{date}`, `{time}`) and cleans the result.
pub fn render_name(pattern: &str, table: &str, ctx: &NameContext) -> String {
    let text = pattern.replace("{connection}", &ctx.connection).replace("{database}", &ctx.database).replace("{table}", table).replace("{tag}", &ctx.tag).replace("{date}", &ctx.date).replace("{time}", &ctx.time);
    let cleaned: String = text.chars().map(|c| if FORBIDDEN.contains(&c) || c.is_control() { '_' } else { c }).collect();
    let trimmed = cleaned.trim().trim_matches('.').to_owned();
    if trimmed.is_empty() { "export".to_owned() } else { trimmed }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> NameContext {
        NameContext { connection: "Prod".into(), database: "shop".into(), tag: "production".into(), date: "20260101".into(), time: "101010".into() }
    }

    /// Every token is replaced.
    #[test]
    fn tokens() {
        assert_eq!(render_name("{connection}_{database}_{table}_{tag}_{date}-{time}", "orders", &ctx()), "Prod_shop_orders_production_20260101-101010");
        assert_eq!(render_name("{table}", "t", &ctx()), "t");
    }

    /// Unsafe characters never reach the file system; empty names fall back.
    #[test]
    fn sanitising() {
        assert_eq!(render_name("{table}", "a/b\\c:d", &ctx()), "a_b_c_d");
        assert_eq!(render_name("../{table}", "x", &ctx()), "_x");
        assert_eq!(render_name("", "x", &ctx()), "export");
        assert_eq!(render_name("{table}", "..", &ctx()), "export");
    }
}
