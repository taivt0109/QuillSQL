//! What the target server can take: The classic compare tool decides parts of the script (and many warnings)
//! from a feature set worked out from the target's version, engine edition and, with
//! UseCompatibilityLevel, the database compatibility level.

use std::collections::BTreeSet;

use crate::catalog::ServerInfo;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Feature {
    CreateOrAlter,
    PolyBase,
    ServiceBroker,
    Sequences,
    SearchPropertyLists,
    ClusteredColumnstore,
    Columnstore,
    MemoryOptimized,
    FileTables,
    Filestream,
    PoisonMessageHandling,
    MultipartSynonyms,
    NonSqlUsers,
    Filegroups,
    IncrementalStatistics,
    /// Triggers can't use WITH APPEND any more.
    NoWithAppend,
    GeographyTessellation,
    TableTypeIndexes,
    SelectiveXmlIndexes,
    UseStatement,
    RowLevelSecurity,
    Stretch,
    DataMasking,
    Temporal,
    TemporalRetention,
    Graph,
    SensitivitySyntax,
    FreeSystemCache,
    EdgeConstraints,
    RecoveryStatusView,
    AlterRole,
    AutoGrid,
    LocalRoutesOnly,
    Online,
    OnlineNonclusteredColumnstore,
    OnlineClusteredColumnstore,
    EntraUsers,
    EntraLogins,
    AzurePolyBase,
    ScalarInlining,
}

/// Version families the classic compare tool tells apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Family {
    Sql2008,
    Sql2008R2,
    Sql2012,
    Sql2012Sp1,
    Sql2014,
    Sql2016,
    Sql2016Sp1,
    Sql2017,
    Sql2019,
    Sql2022,
    AzureSqlDatabase,
    ManagedInstance,
}

#[derive(Clone, Debug)]
pub struct Features {
    pub family: Family,
    set: BTreeSet<Feature>,
    /// Engine edition number (SERVERPROPERTY('EngineEdition')); 0 = unknown.
    pub engine_edition: u32,
}

use Feature::*;

const BASE: &[Feature] = &[MultipartSynonyms, NonSqlUsers, Filegroups, UseStatement, ServiceBroker, Filestream];
const R2: &[Feature] = &[PoisonMessageHandling];
const V2012: &[Feature] = &[
    Sequences,
    SearchPropertyLists,
    Columnstore,
    FileTables,
    NoWithAppend,
    GeographyTessellation,
    FreeSystemCache,
    RecoveryStatusView,
    AlterRole,
    AutoGrid,
];
const V2012_SP1: &[Feature] = &[SelectiveXmlIndexes];
const V2014: &[Feature] = &[ClusteredColumnstore, MemoryOptimized, IncrementalStatistics, TableTypeIndexes];
const V2016: &[Feature] = &[PolyBase, RowLevelSecurity, Stretch, DataMasking, Temporal];
const V2016_SP1: &[Feature] = &[CreateOrAlter];
const V2017: &[Feature] = &[TemporalRetention, Graph];
const V2019: &[Feature] = &[SensitivitySyntax, EdgeConstraints, ScalarInlining];
const AZURE: &[Feature] = &[
    GeographyTessellation,
    Sequences,
    Columnstore,
    ClusteredColumnstore,
    SelectiveXmlIndexes,
    NoWithAppend,
    IncrementalStatistics,
    TableTypeIndexes,
    RowLevelSecurity,
    DataMasking,
    Temporal,
    TemporalRetention,
    SensitivitySyntax,
    Graph,
    EdgeConstraints,
    AlterRole,
    FreeSystemCache,
    AutoGrid,
    Online,
    OnlineNonclusteredColumnstore,
    OnlineClusteredColumnstore,
    EntraUsers,
    PolyBase,
    AzurePolyBase,
];
const MANAGED: &[Feature] = &[
    UseStatement,
    ServiceBroker,
    MultipartSynonyms,
    Filegroups,
    SearchPropertyLists,
    PoisonMessageHandling,
    RecoveryStatusView,
    LocalRoutesOnly,
    EntraUsers,
    EntraLogins,
];

impl Features {
    /// Features of a target. `major` overrides `server.major_version` when the model has none
    /// (snapshots written before the server was recorded); 0 everywhere means "latest".
    pub fn of(server: &ServerInfo, major: u32, compatibility_level: u32, use_compatibility_level: bool) -> Features {
        let family = family(server, major, compatibility_level, use_compatibility_level);
        let mut set = BTreeSet::new();
        let on_prem: &[&[Feature]] = match family {
            Family::AzureSqlDatabase => &[AZURE],
            Family::ManagedInstance => &[AZURE, MANAGED],
            Family::Sql2008 => &[BASE],
            Family::Sql2008R2 => &[BASE, R2],
            Family::Sql2012 => &[BASE, R2, V2012],
            Family::Sql2012Sp1 => &[BASE, R2, V2012, V2012_SP1],
            Family::Sql2014 => &[BASE, R2, V2012, V2012_SP1, V2014],
            Family::Sql2016 => &[BASE, R2, V2012, V2012_SP1, V2014, V2016],
            Family::Sql2016Sp1 => &[BASE, R2, V2012, V2012_SP1, V2014, V2016, V2016_SP1],
            Family::Sql2017 => &[BASE, R2, V2012, V2012_SP1, V2014, V2016, V2016_SP1, V2017],
            Family::Sql2019 | Family::Sql2022 => &[BASE, R2, V2012, V2012_SP1, V2014, V2016, V2016_SP1, V2017, V2019],
        };
        for group in on_prem {
            set.extend(group.iter().copied());
        }
        // Enterprise (or unknown) edition. The classic compare tool has no enterprise entry for 2016 SP1.
        if matches!(server.engine_edition, 0 | 3) {
            match family {
                Family::Sql2019 | Family::Sql2022 => set.extend([Online, OnlineNonclusteredColumnstore, OnlineClusteredColumnstore]),
                Family::Sql2017 => set.extend([Online, OnlineNonclusteredColumnstore]),
                Family::Sql2016Sp1 | Family::AzureSqlDatabase | Family::ManagedInstance => {}
                _ => {
                    set.insert(Online);
                }
            }
        }
        Features { family, set, engine_edition: server.engine_edition }
    }

    /// Everything (a target whose version is unknown).
    pub fn latest() -> Features {
        Features::of(&ServerInfo::default(), 0, 0, false)
    }

    pub fn has(&self, f: Feature) -> bool {
        self.set.contains(&f)
    }

    pub fn is_azure(&self) -> bool {
        matches!(self.family, Family::AzureSqlDatabase | Family::ManagedInstance)
    }

    /// The edition as the classic compare tool names it in messages ("Standard", "Express"…).
    pub fn edition_name(&self) -> &'static str {
        match self.engine_edition {
            1 => "Personal",
            2 => "Standard",
            3 => "Enterprise",
            4 => "Express",
            5 => "AzureSqlDatabase",
            6 => "AzureDataWarehouse",
            8 => "AzureManagedInstance",
            _ => "Unknown",
        }
    }

    /// "2016", "2019"… for messages.
    pub fn version_name(&self) -> &'static str {
        match self.family {
            Family::Sql2008 => "2008",
            Family::Sql2008R2 => "2008 R2",
            Family::Sql2012 | Family::Sql2012Sp1 => "2012",
            Family::Sql2014 => "2014",
            Family::Sql2016 | Family::Sql2016Sp1 => "2016",
            Family::Sql2017 => "2017",
            Family::Sql2019 => "2019",
            Family::Sql2022 => "2022",
            Family::AzureSqlDatabase => "Azure SQL Database",
            Family::ManagedInstance => "Azure SQL Managed Instance",
        }
    }
}

fn family(server: &ServerInfo, major: u32, compatibility_level: u32, use_compatibility_level: bool) -> Family {
    match server.engine_edition {
        5 => return Family::AzureSqlDatabase,
        8 => return Family::ManagedInstance,
        _ => {}
    }
    let major = if server.major_version > 0 { server.major_version } else { major };
    if use_compatibility_level && major > 0 && compatibility_level > 0 && compatibility_level < major * 10 {
        return match compatibility_level {
            150 => Family::Sql2019,
            140 => Family::Sql2017,
            130 => Family::Sql2016Sp1,
            120 => Family::Sql2014,
            110 => Family::Sql2012Sp1,
            100 => Family::Sql2008R2,
            90 | 80 => Family::Sql2008,
            _ => Family::Sql2022,
        };
    }
    // Build and minor parts of "13.0.4001.0".
    let parts: Vec<u32> = server.product_version.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    let minor = parts.get(1).copied().unwrap_or(0);
    let build = parts.get(2).copied().unwrap_or(u32::MAX);
    match major {
        0 => Family::Sql2022,
        15 => Family::Sql2019,
        14 => Family::Sql2017,
        13 if build < 4001 => Family::Sql2016,
        13 => Family::Sql2016Sp1,
        12 => Family::Sql2014,
        11 if build < 3000 => Family::Sql2012,
        11 => Family::Sql2012Sp1,
        10 if minor < 50 => Family::Sql2008,
        10 => Family::Sql2008R2,
        m if m < 10 => Family::Sql2008,
        _ => Family::Sql2022,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(version: &str, edition: u32) -> ServerInfo {
        ServerInfo {
            major_version: version.split('.').next().unwrap().parse().unwrap(),
            product_version: version.into(),
            edition: String::new(),
            engine_edition: edition,
        }
    }

    #[test]
    fn families_and_features() {
        let f = Features::of(&server("13.0.1601.5", 2), 0, 130, false);
        assert_eq!(f.family, Family::Sql2016);
        assert!(f.has(Temporal) && !f.has(CreateOrAlter) && !f.has(Online));
        let f = Features::of(&server("13.0.4001.0", 3), 0, 130, false);
        assert_eq!(f.family, Family::Sql2016Sp1);
        assert!(f.has(CreateOrAlter) && !f.has(Online), "no enterprise features for 2016 SP1");
        let f = Features::of(&server("16.0.4135.4", 3), 0, 160, false);
        assert!(f.has(Online) && f.has(OnlineClusteredColumnstore) && f.has(EdgeConstraints));
        let f = Features::of(&server("12.0.2000.8", 4), 0, 120, false);
        assert!(f.has(MemoryOptimized) && !f.has(Temporal) && !f.has(Online));
    }

    #[test]
    fn compatibility_level_lowers_the_family() {
        let s = server("16.0.4135.4", 2);
        assert_eq!(Features::of(&s, 0, 120, false).family, Family::Sql2022);
        let f = Features::of(&s, 0, 120, true);
        assert_eq!(f.family, Family::Sql2014);
        assert!(!f.has(Temporal));
        // The native level does not lower anything.
        assert_eq!(Features::of(&s, 0, 160, true).family, Family::Sql2022);
    }

    #[test]
    fn azure() {
        let f = Features::of(&server("12.0.2000.8", 5), 0, 150, true);
        assert_eq!(f.family, Family::AzureSqlDatabase);
        assert!(!f.has(UseStatement) && !f.has(Filegroups) && !f.has(CreateOrAlter) && f.has(Temporal) && f.has(Online));
        let f = Features::of(&server("12.0.2000.8", 8), 0, 150, false);
        assert!(f.has(UseStatement) && f.has(ServiceBroker));
        assert!(Features::latest().has(Online));
    }
}
