//! Driver capabilities. All UI enable/disable decisions are driven by this struct.

use crate::dialect::DriverId;

/// What a driver supports (SPEC 5.2, subset for the first run).
#[derive(Debug, Clone, PartialEq)]
pub struct Capabilities {
    pub schemas: bool,
    pub databases_list: bool,
    pub create_db: bool,
    pub rename_db: bool,
    pub drop_db: bool,
    pub encoding_collation_on_create_db: bool,
    pub functions: bool,
    pub procedures: bool,
    pub views: bool,
    pub materialized_views: bool,
    pub sequences: bool,
    pub triggers: bool,
    pub index_tab: bool,
    pub alter_column: bool,
    pub reorder_columns: bool,
    pub add_fk_after_create: bool,
    pub truncate_cascade: bool,
    pub truncate_restart_identity: bool,
    pub drop_cascade: bool,
    pub ilike: bool,
    pub upsert_on_duplicate: bool,
    pub replace_into: bool,
    pub estimated_count: bool,
    pub query_timeout_supported: bool,
    pub transactions: bool,
    pub ssl: bool,
    pub process_list: bool,
}

impl Capabilities {
    /// Reference capability values for a driver (SPEC 5.2 and appendix A).
    pub fn for_driver(id: DriverId) -> Self {
        let base = Self {
            schemas: false,
            databases_list: true,
            create_db: true,
            rename_db: true,
            drop_db: true,
            encoding_collation_on_create_db: false,
            functions: false,
            procedures: false,
            views: true,
            materialized_views: false,
            sequences: false,
            triggers: true,
            index_tab: true,
            alter_column: true,
            reorder_columns: false,
            add_fk_after_create: true,
            truncate_cascade: false,
            truncate_restart_identity: false,
            drop_cascade: false,
            ilike: false,
            upsert_on_duplicate: true,
            replace_into: false,
            estimated_count: true,
            query_timeout_supported: true,
            transactions: true,
            ssl: true,
            process_list: false,
        };
        match id {
            DriverId::Sqlite => Self {
                add_fk_after_create: false,
                estimated_count: false,
                query_timeout_supported: false,
                ssl: false,
                ..base
            },
            DriverId::Postgres => Self {
                schemas: true,
                functions: true,
                procedures: true,
                materialized_views: true,
                sequences: true,
                truncate_cascade: true,
                truncate_restart_identity: true,
                drop_cascade: true,
                ilike: true,
                process_list: true,
                ..base
            },
            DriverId::SqlServer => Self {
                schemas: true,
                functions: true,
                procedures: true,
                sequences: true,
                upsert_on_duplicate: false,
                process_list: true,
                ..base
            },
            DriverId::MySql => Self {
                rename_db: false,
                encoding_collation_on_create_db: true,
                functions: true,
                procedures: true,
                reorder_columns: true,
                replace_into: true,
                process_list: true,
                ..base
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference values from SPEC 5.2.
    #[test]
    fn reference_capabilities() {
        assert!(!Capabilities::for_driver(DriverId::Sqlite).add_fk_after_create);
        assert!(!Capabilities::for_driver(DriverId::MySql).rename_db);
        assert!(Capabilities::for_driver(DriverId::MySql).reorder_columns);
        assert!(Capabilities::for_driver(DriverId::Postgres).schemas);
        assert!(Capabilities::for_driver(DriverId::Postgres).ilike);
        assert!(!Capabilities::for_driver(DriverId::Sqlite).process_list);
        let ms = Capabilities::for_driver(DriverId::SqlServer);
        assert!(ms.schemas && ms.functions && ms.procedures && ms.process_list && ms.triggers);
        assert!(!ms.materialized_views && !ms.ilike && !ms.upsert_on_duplicate && !ms.replace_into);
        assert!(!ms.reorder_columns && !ms.truncate_cascade && !ms.drop_cascade);
    }
}
