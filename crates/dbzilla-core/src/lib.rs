//! DBZilla core: domain types, dialects and pure SQL builders. No UI, no I/O.

pub mod caps;
pub mod dialect;
pub mod model;
pub mod value;
pub mod splitter;
pub mod safemode;
pub mod changes;
pub mod filter;
pub mod copyfmt;
pub mod ddl;
pub mod sqlite_rewrite;
pub mod url;
pub mod paging;
pub mod rowlimit;
pub mod params;
pub mod diagram;
