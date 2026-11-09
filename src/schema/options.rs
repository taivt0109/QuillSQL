//! Schema compare options: one table that the engine, the command line and the Options tab
//! all read, so they cannot drift apart. Names match the classic compare tool's `/options` names so that
//! existing command lines keep working.

use std::collections::BTreeSet;
use std::fmt;

/// Where an option sits on the Options tab; `Hidden` options are command-line only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Behavior,
    Ignore,
    Hidden,
}

/// What an option changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The comparison result (and usually what gets deployed).
    Compare,
    /// Only the deployment script.
    Deploy,
    /// How a database is read.
    Read,
}

macro_rules! options {
    ($( $variant:ident = $group:ident $kind:ident $default:literal $label:literal; )*) => {
        /// One engine flag.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Opt { $( $variant, )* }

        /// Every flag, in Options tab order (hidden flags last).
        pub const ALL: &[Opt] = &[ $( Opt::$variant, )* ];

        impl Opt {
            pub fn name(self) -> &'static str {
                match self { $( Opt::$variant => stringify!($variant), )* }
            }

            /// Text of the check box on the Options tab.
            pub fn label(self) -> &'static str {
                match self { $( Opt::$variant => $label, )* }
            }

            pub fn group(self) -> Group {
                match self { $( Opt::$variant => Group::$group, )* }
            }

            pub fn kind(self) -> Kind {
                match self { $( Opt::$variant => Kind::$kind, )* }
            }

            /// Part of the default set (the same nine as the classic compare tool's defaults, plus IncludeDependencies).
            pub fn is_default(self) -> bool {
                match self { $( Opt::$variant => $default, )* }
            }
        }
    };
}

options! {
    AddDatabaseUseStatement = Behavior Deploy false "Add database USE statement";
    ObjectExistenceChecks = Behavior Deploy false "Add object existence checks";
    AddWithEncryption = Behavior Deploy false "Add WITH ENCRYPTION";
    NoAutoColumnMapping = Behavior Compare false "Auto-map similar columns";
    ConsiderNextFilegroupInPartitionSchemes = Behavior Compare false "Consider next filegroups in partition schemes";
    SsdtCompatibleOptions = Behavior Read false "Database Project compatible script folder output";
    DecryptPost2kEncryptedObjects = Behavior Read true "Decrypt encrypted objects";
    DisableAndReenableDdlTriggers = Behavior Deploy false "Disable DDL triggers during deployment";
    NoErrorHandling = Behavior Deploy false "Don't add error handling statements to deployment scripts";
    DoNotOutputCommentHeader = Behavior Deploy false "Don't include comment header in the deployment script";
    UseClrUdtToStringForClrMigration = Behavior Deploy false "Don't use ALTER ASSEMBLY to change CLR objects";
    NoTransactions = Behavior Deploy false "Don't use transactions in deployment scripts";
    NoDeploymentLogging = Behavior Deploy false "Log the deployment in the target database";
    ForceColumnOrder = Behavior Compare false "Force column order";
    IgnoreMigrationScripts = Behavior Read false "Ignore migration scripts for databases";
    CaseSensitiveObjectDefinition = Behavior Compare false "Use case-sensitive object definition";
    UseCompatibilityLevel = Behavior Read false "Use database compatibility level";
    DropAndCreateInsteadOfAlter = Behavior Deploy false "Add DROP and CREATE for rerunnable scripts";
    CreateOrAlterInsteadOfAlter = Behavior Deploy false "Add CREATE OR ALTER for rerunnable scripts where possible";
    AddNoPopulation = Behavior Deploy false "Add NO POPULATION to fulltext indexes";
    AddOnlineOn = Behavior Deploy false "Add ONLINE = ON when creating indexes or altering columns";

    IgnoreSchemaObjectAuthorization = Ignore Compare false "Ignore authorization on schema objects";
    IgnoreBindings = Ignore Compare false "Ignore bindings";
    IgnoreCertificatesAndCryptoKeys = Ignore Compare false "Ignore certificates, symmetric keys and asymmetric keys";
    IgnoreChangeTracking = Ignore Compare false "Ignore change tracking";
    IgnoreChecks = Ignore Compare false "Ignore check constraints";
    IgnoreCollations = Ignore Compare false "Ignore collations";
    IgnoreComments = Ignore Compare false "Ignore comments";
    IgnoreConstraintNames = Ignore Compare false "Ignore constraint and index names";
    IgnoreDataCompression = Ignore Compare false "Ignore data compression";
    IgnoreDatabaseAndServerName = Ignore Compare true "Ignore database and server name in synonyms";
    IgnoreTriggerOrder = Ignore Compare false "Ignore DML trigger order";
    IgnoreTriggers = Ignore Compare false "Ignore DML triggers";
    IgnoreQueueEventNotifications = Ignore Compare false "Ignore event notification on queues";
    IgnoreExtendedProperties = Ignore Compare false "Ignore extended properties";
    IgnoreFileGroups = Ignore Compare true "Ignore filegroups, partition schemes and partition functions";
    IgnoreFillFactor = Ignore Compare true "Ignore fill factor and index padding";
    IgnoreKeys = Ignore Compare false "Ignore foreign keys";
    IgnoreFullTextIndexing = Ignore Compare false "Ignore full-text indexing";
    IgnoreIdentityProperties = Ignore Compare false "Ignore identity property on columns";
    IgnoreIdentitySeedAndIncrement = Ignore Compare false "Ignore identity seed and increment values";
    IgnoreIndexes = Ignore Compare false "Ignore indexes";
    IgnoreInsteadOfTriggers = Ignore Compare false "Ignore INSTEAD OF triggers";
    IgnoreIndexLockProperties = Ignore Compare false "Ignore LOCK properties of indexes";
    IgnoreNocheckAndWithNocheck = Ignore Compare false "Ignore NOCHECK and WITH NOCHECK";
    IgnoreNotForReplication = Ignore Compare false "Ignore NOT FOR REPLICATION";
    IgnoreNullability = Ignore Compare false "Ignore nullability of columns";
    IgnorePerformanceIndexes = Ignore Compare false "Ignore performance indexes";
    IgnorePermissions = Ignore Compare false "Ignore permissions";
    IgnoreReplicationTriggers = Ignore Compare false "Ignore replication triggers";
    IgnoreQuotedIdentifiersAndAnsiNullSettings = Ignore Compare false "Ignore SET QUOTED_IDENTIFIER and SET ANSI_NULLS statements";
    IgnoreSensitivityClassification = Ignore Compare false "Ignore sensitivity classifications";
    IgnoreSquareBrackets = Ignore Compare false "Ignore square brackets in object names";
    IgnoreStatistics = Ignore Compare false "Ignore statistics";
    IgnoreStatisticsNorecompute = Ignore Compare false "Ignore STATISTICS_NORECOMPUTE property on indexes";
    IgnoreSystemNamedConstraintNames = Ignore Compare false "Ignore system named constraint and index names";
    IgnoreTSQLT = Ignore Compare false "Ignore tSQLt framework and tests";
    IgnoreUserProperties = Ignore Compare true "Ignore user properties";
    IgnoreUsers = Ignore Compare false "Ignore users' permissions and role memberships";
    IgnoreWhiteSpace = Ignore Compare true "Ignore white space";
    IgnoreWithElementOrder = Ignore Compare true "Ignore WITH element order";
    IgnoreWithEncryption = Ignore Compare false "Ignore WITH ENCRYPTION";
    IgnoreWithNocheck = Ignore Compare false "Ignore WITH NOCHECK";
    IgnoreDynamicDataMasking = Ignore Compare false "Ignore dynamic data masking";
    IgnoreStatisticsIncremental = Ignore Compare false "Ignore STATISTICS_INCREMENTAL";

    IncludeDependencies = Hidden Deploy true "Include dependencies";
    IgnoreSynonymDependencies = Hidden Compare false "Ignore synonym dependencies";
    IgnoreOwners = Hidden Compare false "Ignore owners";
    ForceSyncScriptGeneration = Hidden Deploy false "Force deployment script generation";
    AssumeAlreadyInTransaction = Hidden Deploy false "Assume the script runs inside a transaction";
    IgnorePrePostScripts = Hidden Deploy false "Ignore pre- and post-deployment scripts";
    IgnoreInternallyUsedMicrosoftExtendedProperties = Hidden Compare false "Ignore internal Microsoft extended properties";
    IgnoreDataSyncSchema = Hidden Compare false "Ignore the DataSync schema";
    InlineTableObjects = Hidden Read false "Script indexes and constraints inside CREATE TABLE";
    InlineFulltextFields = Hidden Read false "Script full-text columns inline";
    InferTypeForSqlVariant = Hidden Read false "Infer the type of sql_variant values";
    UseSetStatementsInScriptDatabaseInfo = Hidden Read false "Use SET statements in database info";
    WriteAssembliesAsDlls = Hidden Read false "Write assemblies as DLL files";
    ReadAsDataToolsPermissions = Hidden Read false "Read permissions as Data Tools does";
    WriteAsDataToolsPermissions = Hidden Read false "Write permissions as Data Tools does";
    ForbidDuplicateTableStorageSettings = Hidden Read false "Forbid duplicate table storage settings";
    ThrowOnFileParseFailed = Hidden Read false "Fail when a script file cannot be parsed";
    SchemaExistenceChecks = Hidden Deploy false "Add schema existence checks";
    RoleExistenceChecks = Hidden Deploy false "Add role existence checks";
    EnableDeployNow = Hidden Deploy false "Enable deploy now";
    CloneDatabaseLevelPropertiesForMigrations = Hidden Deploy false "Clone database-level properties for migrations";
}

impl Opt {
    /// The Options tab shows this flag inverted ("Auto-map similar columns" = not NoAutoColumnMapping).
    pub fn presented_as_negation(self) -> bool {
        matches!(self, Opt::NoAutoColumnMapping | Opt::NoDeploymentLogging)
    }

    /// Ticking this option also ticks these; clearing one of them clears this.
    pub fn requires(self) -> &'static [Opt] {
        match self {
            Opt::DropAndCreateInsteadOfAlter => &[Opt::ObjectExistenceChecks],
            Opt::SsdtCompatibleOptions => &[
                Opt::IgnorePermissions,
                Opt::IgnoreQuotedIdentifiersAndAnsiNullSettings,
                Opt::IgnoreWithNocheck,
                Opt::IgnoreSystemNamedConstraintNames,
            ],
            Opt::IgnoreNocheckAndWithNocheck => &[Opt::IgnoreWithNocheck],
            Opt::NoErrorHandling => &[Opt::NoTransactions],
            Opt::IgnoreIndexes => &[Opt::IgnorePerformanceIndexes],
            _ => &[],
        }
    }

    /// A group name standing for several flags.
    fn group_members(self) -> &'static [Opt] {
        match self {
            Opt::SsdtCompatibleOptions => &[
                Opt::SsdtCompatibleOptions,
                Opt::IgnorePermissions,
                Opt::IgnoreQuotedIdentifiersAndAnsiNullSettings,
                Opt::IgnoreWithNocheck,
                Opt::IgnoreSystemNamedConstraintNames,
                Opt::InlineTableObjects,
                Opt::InlineFulltextFields,
                Opt::InferTypeForSqlVariant,
                Opt::UseSetStatementsInScriptDatabaseInfo,
                Opt::WriteAssembliesAsDlls,
                Opt::ReadAsDataToolsPermissions,
                Opt::WriteAsDataToolsPermissions,
                Opt::ForbidDuplicateTableStorageSettings,
            ],
            _ => &[],
        }
    }
}

/// Names the classic compare tool's command line accepts besides the flag names themselves.
const ALIASES: &[(&str, &[Opt])] = &[
    ("NoSQLPlumbing", &[Opt::NoTransactions, Opt::NoErrorHandling, Opt::NoDeploymentLogging]),
    ("OnlineIndexBuild", &[Opt::AddOnlineOn]),
    ("IgnoreDatabaseName", &[Opt::IgnoreDatabaseAndServerName]),
    ("DisableSocForLiveDbs", &[Opt::IgnoreMigrationScripts]),
    ("IgnoreUsersPermissionsAndRoleMemberships", &[Opt::IgnoreUsers]),
    ("UseCaseSensitiveObjectDefinition", &[Opt::CaseSensitiveObjectDefinition]),
    ("IgnoreForeignKeys", &[Opt::IgnoreKeys]),
    ("IgnoreCheckConstraints", &[Opt::IgnoreChecks]),
    ("IgnoreFileGroupsPartitionSchemesAndPartitionFunctions", &[Opt::IgnoreFileGroups]),
    ("IgnoreIdentitySeedAndIncrementValues", &[Opt::IgnoreIdentitySeedAndIncrement]),
    ("IgnoreConstraintAndIndexNames", &[Opt::IgnoreConstraintNames]),
    ("IgnoreSystemNamedConstraintAndIndexNames", &[Opt::IgnoreSystemNamedConstraintNames]),
    ("IgnoreEventNotificationsOnQueues", &[Opt::IgnoreQueueEventNotifications]),
    ("DontAlterAssembly", &[Opt::UseClrUdtToStringForClrMigration]),
    ("IgnoreLockPropertiesOfIndexes", &[Opt::IgnoreIndexLockProperties]),
    ("IgnoreIdentityPropertiesOnColumns", &[Opt::IgnoreIdentityProperties]),
    ("IgnoreDatabaseAndServerNameInSynonyms", &[Opt::IgnoreDatabaseAndServerName]),
    ("DecryptEncryptedObjects", &[Opt::DecryptPost2kEncryptedObjects]),
    ("DropAndCreateForReRunnableScripts", &[Opt::DropAndCreateInsteadOfAlter]),
    ("CreateOrAlterForReRunnableScripts", &[Opt::CreateOrAlterInsteadOfAlter]),
    ("AddObjectExistenceChecks", &[Opt::ObjectExistenceChecks]),
    ("IgnoreFillFactorAndIndexPadding", &[Opt::IgnoreFillFactor]),
    ("IgnoreWhitespace", &[Opt::IgnoreWhiteSpace]),
];

/// A set of flags.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options(BTreeSet<Opt>);

impl Options {
    pub fn none() -> Self {
        Options(BTreeSet::new())
    }

    pub fn default_set() -> Self {
        Options(ALL.iter().copied().filter(|o| o.is_default()).collect())
    }

    pub fn has(&self, o: Opt) -> bool {
        self.0.contains(&o)
    }

    pub fn with(mut self, o: Opt) -> Self {
        self.set(o, true);
        self
    }

    pub fn without(mut self, o: Opt) -> Self {
        self.set(o, false);
        self
    }

    /// Sets or clears a flag, a group's members, and the flags it requires (setting) or that
    /// require it (clearing), as the Options tab does.
    pub fn set(&mut self, o: Opt, on: bool) {
        let members = o.group_members();
        let flags: Vec<Opt> = if members.is_empty() { vec![o] } else { members.to_vec() };
        for f in flags {
            if on {
                if self.0.insert(f) {
                    for r in f.requires() {
                        self.set(*r, true);
                    }
                }
            } else if self.0.remove(&f) {
                for dependent in ALL.iter().filter(|d| d.requires().contains(&f)) {
                    self.set(*dependent, false);
                }
            }
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = Opt> + '_ {
        self.0.iter().copied()
    }

    /// Checks combinations the deployment cannot honour.
    pub fn validate(&self) -> Result<(), String> {
        if self.has(Opt::NoErrorHandling) && !self.has(Opt::NoTransactions) {
            return Err("You must also specify the NoTransactions option when you use the NoErrorHandling option.".into());
        }
        Ok(())
    }

    /// Parses a `/options` value: names separated by `,` or `|`, case-insensitive.
    /// As in the classic compare tool, a list without `Default` starts from no options at all.
    pub fn parse(list: &str) -> Result<Options, String> {
        let names: Vec<&str> = list.split([',', '|']).map(str::trim).filter(|s| !s.is_empty()).collect();
        let mut out = if names.iter().any(|n| n.eq_ignore_ascii_case("Default")) {
            Options::default_set()
        } else {
            Options::none()
        };
        for name in names {
            let name = name.strip_prefix("Options.").unwrap_or(name);
            if name.eq_ignore_ascii_case("Default") || name.eq_ignore_ascii_case("None") {
                continue;
            }
            if name.eq_ignore_ascii_case("All") {
                for o in ALL {
                    out.0.insert(*o);
                }
                continue;
            }
            // Accepted and ignored for compatibility.
            if name.to_ascii_lowercase().starts_with("targetispre") || name.eq_ignore_ascii_case("TargetIsSqlAzure") {
                continue;
            }
            if let Some(o) = ALL.iter().find(|o| o.name().eq_ignore_ascii_case(name)) {
                out.set(*o, true);
            } else if let Some((_, flags)) = ALIASES.iter().find(|(a, _)| a.eq_ignore_ascii_case(name)) {
                for f in *flags {
                    out.set(*f, true);
                }
            } else {
                return Err(format!("unknown option '{name}'"));
            }
        }
        Ok(out)
    }
}

impl fmt::Display for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return write!(f, "None");
        }
        let names: Vec<&str> = self.0.iter().map(|o| o.name()).collect();
        write!(f, "{}", names.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_sql_compare() {
        let d = Options::default_set();
        let names: Vec<&str> = d.iter().map(Opt::name).collect();
        assert_eq!(
            names,
            [
                "DecryptPost2kEncryptedObjects",
                "IgnoreDatabaseAndServerName",
                "IgnoreFileGroups",
                "IgnoreFillFactor",
                "IgnoreUserProperties",
                "IgnoreWhiteSpace",
                "IgnoreWithElementOrder",
                "IncludeDependencies"
            ]
        );
    }

    #[test]
    fn options_tab_has_65_boxes() {
        assert_eq!(ALL.iter().filter(|o| o.group() == Group::Behavior).count(), 21);
        assert_eq!(ALL.iter().filter(|o| o.group() == Group::Ignore).count(), 44);
    }

    #[test]
    fn parses_cli_lists() {
        let o = Options::parse("Default,IgnoreForeignKeys, ignorecomments").unwrap();
        assert!(o.has(Opt::IgnoreKeys) && o.has(Opt::IgnoreComments) && o.has(Opt::IgnoreWhiteSpace));
        let o = Options::parse("IgnoreComments").unwrap();
        assert!(!o.has(Opt::IgnoreWhiteSpace), "a list without Default starts empty");
        let o = Options::parse("NoSQLPlumbing").unwrap();
        assert!(o.has(Opt::NoTransactions) && o.has(Opt::NoErrorHandling));
        assert!(Options::parse("Bogus").is_err());
        assert!(Options::parse("TargetIsPre2016").unwrap().iter().next().is_none());
    }

    #[test]
    fn requirements_cascade() {
        let o = Options::none().with(Opt::DropAndCreateInsteadOfAlter);
        assert!(o.has(Opt::ObjectExistenceChecks));
        let o = o.without(Opt::ObjectExistenceChecks);
        assert!(!o.has(Opt::DropAndCreateInsteadOfAlter));
        let o = Options::none().with(Opt::SsdtCompatibleOptions);
        assert!(o.has(Opt::InlineTableObjects) && o.has(Opt::IgnorePermissions));
        assert!(Options::none().with(Opt::NoErrorHandling).validate().is_ok());
        let mut o = Options::none();
        o.0.insert(Opt::NoErrorHandling);
        assert!(o.validate().is_err());
    }
}
