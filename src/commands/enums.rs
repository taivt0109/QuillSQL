//! Values of the classic compare tool's enumerated switches: the canonical name first, then its aliases.
//! Values outside a list are refused (exit 34); values inside it that QuillSQL does not
//! implement are accepted and have no effect, as the classic compare tool does with names its engine lacks.

/// `/options` of `quill schema`.
pub const SCHEMA_OPTIONS: &[&[&str]] = &[
    &["None", "n"],
    &["Default", "d"],
    &["ForceColumnOrder", "f"],
    &["IgnoreFillFactor", "if"],
    &["IgnorePermissions", "ip"],
    &["IgnoreWhiteSpace", "iw"],
    &["IgnoreBindings", "ib"],
    &["IgnoreQuotedIdentifiersAndAnsiNullSettings", "iq"],
    &["UseCaseSensitiveObjectDefinition", "cs", "CaseSensitiveObjectDefinition"],
    &["IgnoreExtendedProperties", "ie"],
    &["IgnoreFullTextIndexing", "ift"],
    &["NoAutoColumnMapping", "nacm"],
    &["NoTransactions", "nt", "NoSQLPlumbing", "np"],
    &["NoErrorHandling", "neh"],
    &["NoDeploymentLogging", "ndl"],
    &["IgnoreCollations", "ic"],
    &["IgnoreComments", "icm"],
    &["IgnoreIndexes", "ii"],
    &["IgnoreForeignKeys", "ifk", "IgnoreKeys", "ik"],
    &["IgnoreCheckConstraints", "ich", "IgnoreChecks"],
    &["IgnoreTriggers", "it"],
    &["IncludeDependencies", "incd"],
    &["IgnoreInsteadOfTriggers", "iit"],
    &["IgnoreFileGroupsPartitionSchemesAndPartitionFunctions", "ifg", "IgnoreFileGroups"],
    &["IgnoreIdentitySeedAndIncrementValues", "isi", "IgnoreIdentitySeedAndIncrement"],
    &["IgnoreWithNocheck", "iwn"],
    &["IgnoreNocheckAndWithNocheck", "inwn"],
    &["IgnoreConstraintAndIndexNames", "icn", "IgnoreConstraintNames"],
    &["IgnoreSystemNamedConstraintAndIndexNames", "iscn", "IgnoreSystemNamedConstraintNames"],
    &["IgnoreStatistics", "ist"],
    &["DoNotOutputCommentHeader", "nc"],
    &["AddWithEncryption", "we"],
    &["AddNoPopulation", "anp"],
    &["IgnoreEventNotificationsOnQueues", "iqen", "IgnoreQueueEventNotifications"],
    &["DontAlterAssembly", "daa", "DontAlterAssemblies", "UseClrUdtToStringForClrMigration", "uclr"],
    &["ConsiderNextFilegroupInPartitionSchemes", "cfgps"],
    &["IgnoreCertificatesAndCryptoKeys", "icc"],
    &["IgnoreTriggerOrder", "ito"],
    &["IgnoreUsersPermissionsAndRoleMemberships", "iu", "IgnoreUsers"],
    &["IgnoreUserProperties", "iup"],
    &["DisableAndReenableDDLTriggers", "drd"],
    &["IgnoreWithElementOrder", "iweo"],
    &["IgnoreLockPropertiesOfIndexes", "ilpi", "IgnoreIndexLockProperties", "IgnoreLockPropertiesOnIndexes", "iilp"],
    &["IgnoreReplicationTriggers", "irpt"],
    &["IgnoreIdentityPropertiesOnColumns", "iip", "IgnoreIdentityProperties"],
    &["IgnoreNotForReplication", "infr"],
    &["IgnoreDataCompression", "idc"],
    &[
        "IgnoreDatabaseAndServerNameInSynonyms",
        "idsn",
        "IgnoreDatabaseAndServerNameInSynonym",
        "IgnoreDatabaseAndServerNamesInSynonyms",
        "IgnoreDatabaseAndServerNamesInSynonym",
        "IgnoreDatabaseAndServerName",
        "IgnoreDatabaseName",
        "idn",
    ],
    &["AddDatabaseUseStatement", "adus"],
    &["IgnoreSchemaObjectAuthorization", "isoa"],
    &["DecryptEncryptedObjects", "deo", "DecryptPost2kEncryptedObjects", "dp2k"],
    &["IgnoreStatisticsNorecompute", "isn"],
    &["IgnoreSquareBrackets", "isb"],
    &["ThrowOnFileParseFailed", "tofpf"],
    &["AddObjectExistenceChecks", "oec", "ObjectExistenceChecks"],
    &["DropAndCreateForReRunnableScripts", "dac", "DropAndCreateInsteadOfAlter", "dacia"],
    &["CreateOrAlterForReRunnableScripts", "coa"],
    &["IgnoreMigrationScripts", "ims", "DisableSocForLiveDbs", "dafld"],
    &["IgnoreTSQLT", "itst"],
    &["IgnoreWithEncryption", "iwe"],
    &["UseCompatibilityLevel", "ucl"],
    &["IgnoreNullability", "in"],
    &["IgnorePerformanceIndexes", "ipi"],
    &["IgnoreChangeTracking", "ict"],
    &["IgnoreSensitivityClassification", "isc"],
    &["IgnoreDynamicDataMasking", "iddm"],
    &["IgnoreStatisticsIncremental", "isinc"],
    &["AddOnlineOn", "oo", "OnlineIndexBuild", "oib"],
];

/// `/datacompareoptions` of `quill schema` and `/options` of `quill data`.
pub const DATA_OPTIONS: &[&[&str]] = &[
    &["None", "n"],
    &["Default", "d"],
    &["DisableAndReenableDDLTriggers", "drd", "DDLTriggerDisable", "ddltd"],
    &["DisableAndReenableDMLTriggers", "t", "DisableDMLTriggers"],
    &["DisableKeys", "k"],
    &["DropConstraintsAndIndexes", "c"],
    &["DontUseTransactions", "nt"],
    &["UseTransactions", "ut"],
    &["AddDatabaseUseStatement", "adus"],
    &["TransportCLRBinary", "tclr"],
    &["OutputComments", "oc"],
    &["ReseedIdentity", "rsi"],
    &["SkipFkChecks", "sc"],
    &["ForceCheck", "fc"],
    &["UseFullPrecisionForComparison", "ufpfc"],
    &["TreatEmptyStringAsNull", "tesan"],
    &["IgnoreCase", "ica"],
    &["CaseSensitiveObjectDefinition", "cs"],
    &["IgnoreSpaces", "is"],
    &["IgnoreUnderscores", "iun"],
    &["IncludeTimestamps", "its"],
    &["IncludeIndexedViews", "v"],
    &["IncludeIdentities", "iid", "eid"],
    &["DoNotOutputCommentHeader", "nc"],
    &["IncludeGeneratedAlwaysColumns", "igac"],
    &["TrimTrailingSpaces", "tts"],
    &["ForceBinaryCollation", "fbc"],
    &["UseChecksumComparison", "ucc"],
    &["CompressTemporaryFiles", "ctf"],
    &["MissingFrom2AsInclude", "mf2ai"],
];

/// Object and difference types of `/include` and `/exclude` in `quill schema`.
pub const SCHEMA_OBJECT_TYPES: &[&str] = &[
    "All",
    "Table",
    "View",
    "StoredProcedure",
    "Default",
    "FullTextCatalog",
    "Function",
    "Role",
    "Rule",
    "User",
    "UserDefinedType",
    "DdlTrigger",
    "Assembly",
    "Synonym",
    "XmlSchemaCollection",
    "MessageType",
    "Contract",
    "Queue",
    "Service",
    "Route",
    "EventNotification",
    "PartitionScheme",
    "PartitionFunction",
    "Schema",
    "ServiceBinding",
    "Certificate",
    "SymmetricKey",
    "AsymmetricKey",
    "FullTextStoplist",
    "ExtendedProperty",
    "Sequence",
    "SearchPropertyList",
    "ExternalDataSource",
    "ExternalFileFormat",
    "ExternalTable",
    "Additional",
    "Different",
    "Missing",
    "Identical",
    "StaticData",
];

pub const REPORT_TYPES: &[&[&str]] = &[&["Xml"], &["Simple"], &["Interactive"], &["Html"], &["Classic"], &["Excel"]];
pub const WARNING_LEVELS: &[&[&str]] = &[&["None", "n"], &["Medium", "m", "true"], &["High", "h"]];
pub const ENCODINGS: &[&[&str]] = &[&["UTF8"], &["UTF8WithPreamble"], &["Unicode"], &["ASCII"]];
pub const LOG_LEVELS: &[&[&str]] = &[&["None", "n"], &["Error", "e"], &["Warning", "w"], &["Verbose", "v"]];
pub const BACKUP_PROVIDERS: &[&[&str]] = &[&["native"], &["sqb"]];
pub const BACKUP_TYPES: &[&[&str]] = &[&["full"], &["differential"]];

/// The canonical names of a `,` list, or the first value that is not in `values`.
pub fn canonical<'a>(values: &[&[&'a str]], list: &str) -> Result<Vec<&'a str>, String> {
    list.split([',', '|'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|v| values.iter().find(|names| names.iter().any(|n| n.eq_ignore_ascii_case(v))).map(|names| names[0]).ok_or_else(|| v.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Options;

    #[test]
    fn every_schema_option_name_is_unique_and_most_are_implemented() {
        let mut seen = std::collections::HashSet::new();
        for names in SCHEMA_OPTIONS {
            for n in *names {
                assert!(seen.insert(n.to_ascii_lowercase()), "{n} twice");
            }
        }
        let missing: Vec<&str> = SCHEMA_OPTIONS.iter().map(|n| n[0]).filter(|n| Options::parse(n).is_err()).collect();
        assert!(missing.is_empty(), "not implemented: {missing:?}");
        assert_eq!(canonical(SCHEMA_OPTIONS, "iw, NoSQLPlumbing,IgnoreKeys").unwrap(), ["IgnoreWhiteSpace", "NoTransactions", "IgnoreForeignKeys"]);
        assert_eq!(canonical(SCHEMA_OPTIONS, "Default,Bogus").unwrap_err(), "Bogus");
    }
}
