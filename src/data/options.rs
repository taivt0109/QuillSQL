//! Data compare options: the 25 check boxes of the Options tab, with SQL Data Compare's names
//! and defaults, shared by the command line and the UI.

use std::collections::BTreeSet;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Mapping,
    Comparison,
    Deployment,
}

macro_rules! data_options {
    ($( $variant:ident = $group:ident $default:literal $label:literal; )*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum DataOpt { $( $variant, )* }

        /// In Options tab order.
        pub const ALL: &[DataOpt] = &[ $( DataOpt::$variant, )* ];

        impl DataOpt {
            pub fn name(self) -> &'static str {
                match self { $( DataOpt::$variant => stringify!($variant), )* }
            }

            pub fn label(self) -> &'static str {
                match self { $( DataOpt::$variant => $label, )* }
            }

            pub fn group(self) -> Group {
                match self { $( DataOpt::$variant => Group::$group, )* }
            }

            pub fn is_default(self) -> bool {
                match self { $( DataOpt::$variant => $default, )* }
            }
        }
    };
}

data_options! {
    IgnoreSpaces = Mapping true "Ignore spaces in object names";
    IgnoreUnderscores = Mapping false "Ignore underscores in object names";
    IncludeIdentities = Mapping true "Include identity columns";
    IncludeTimestamps = Mapping true "Include timestamp columns";
    IncludeViews = Mapping false "Include views";
    CaseSensitiveObjectDefinition = Mapping false "Use case-sensitive object definition";
    IncludeGeneratedAlwaysColumns = Mapping false "Include generated always columns";
    MissingFrom2AsInclude = Mapping false "Compare tables missing from database 2 as empty";

    CompressTemporaryFiles = Comparison false "Compress temporary files";
    ForceBinaryCollation = Comparison false "Force binary collation (case-sensitive)";
    IncludeIdenticalRecords = Comparison false "Show identical values in results";
    TreatEmptyStringAsNull = Comparison false "Treat empty string as NULL";
    TrimTrailingSpaces = Comparison false "Trim trailing white space";
    UseChecksumComparison = Comparison false "Use checksum comparison";
    UseFullPrecisionForComparison = Comparison false "Use maximum precision for floats";

    DisableDdlTriggers = Deployment false "Disable DDL triggers";
    DisableTriggers = Deployment false "Disable DML triggers";
    DisableKeys = Deployment true "Disable foreign keys";
    DontOutputComments = Deployment false "Don't include comments in the deployment script";
    DontUseTransactions = Deployment false "Don't use transactions in the deployment script";
    DropConstraintsAndIndexes = Deployment false "Drop primary keys, indexes, and unique constraints";
    AddDatabaseUseStatement = Deployment false "Add database USE statement";
    OutputCommentHeader = Deployment true "Include comment header in the deployment script";
    ReseedIdentity = Deployment false "Reseed identity columns";
    SkipFkChecks = Deployment false "Skip integrity checks for foreign key constraints";
    TransportClrBinary = Deployment false "Transport CLR data types as binary";
}

/// SQL Data Compare's own names for the same flags (its enum names and command-line aliases).
const ALIASES: &[(&str, DataOpt)] = &[
    ("IncludeIndexedViews", DataOpt::IncludeViews),
    ("DDLTriggerDisable", DataOpt::DisableDdlTriggers),
    ("DisableAndReenableDDLTriggers", DataOpt::DisableDdlTriggers),
    ("DisableAndReenableDMLTriggers", DataOpt::DisableTriggers),
    ("TransportCLRBinary", DataOpt::TransportClrBinary),
    ("IgnoreSpacesInComparison", DataOpt::IgnoreSpaces),
    ("ForceBinaryCollation", DataOpt::ForceBinaryCollation),
    ("TrimTrailingWhiteSpace", DataOpt::TrimTrailingSpaces),
    ("TrimTrailingSpaces", DataOpt::TrimTrailingSpaces),
    ("CompareUsingChecksum", DataOpt::UseChecksumComparison),
    ("IncludeIdentical", DataOpt::IncludeIdenticalRecords),
    ("DisableForeignKeys", DataOpt::DisableKeys),
    ("DisableDMLTriggers", DataOpt::DisableTriggers),
    ("DisableDDLTriggers", DataOpt::DisableDdlTriggers),
    ("NoTransactions", DataOpt::DontUseTransactions),
    ("DropPrimaryKeysIndexesAndUniqueConstraints", DataOpt::DropConstraintsAndIndexes),
    ("ReseedIdentityColumns", DataOpt::ReseedIdentity),
    ("SkipIntegrityChecks", DataOpt::SkipFkChecks),
    ("TreatEmptyStringAsNull", DataOpt::TreatEmptyStringAsNull),
    ("UseMaximumPrecisionForFloats", DataOpt::UseFullPrecisionForComparison),
    ("OutputCommentHeader", DataOpt::OutputCommentHeader),
    ("IncludeTimestamps", DataOpt::IncludeTimestamps),
    ("IncludeIdentities", DataOpt::IncludeIdentities),
    ("mf2ai", DataOpt::MissingFrom2AsInclude),
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DataOptions(BTreeSet<DataOpt>);

impl DataOptions {
    pub fn none() -> Self {
        DataOptions(BTreeSet::new())
    }

    pub fn default_set() -> Self {
        DataOptions(ALL.iter().copied().filter(|o| o.is_default()).collect())
    }

    pub fn has(&self, o: DataOpt) -> bool {
        self.0.contains(&o)
    }

    pub fn with(mut self, o: DataOpt) -> Self {
        self.0.insert(o);
        // "Show identical values" and "checksum comparison" exclude each other.
        match o {
            DataOpt::IncludeIdenticalRecords => {
                self.0.remove(&DataOpt::UseChecksumComparison);
            }
            DataOpt::UseChecksumComparison => {
                self.0.remove(&DataOpt::IncludeIdenticalRecords);
            }
            _ => {}
        }
        self
    }

    pub fn without(mut self, o: DataOpt) -> Self {
        self.0.remove(&o);
        self
    }

    pub fn iter(&self) -> impl Iterator<Item = DataOpt> + '_ {
        self.0.iter().copied()
    }

    /// A `/options` list: names separated by `,`; a list without `Default` starts from none.
    pub fn parse(list: &str) -> Result<DataOptions, String> {
        let names: Vec<&str> = list.split([',', '|']).map(str::trim).filter(|s| !s.is_empty()).collect();
        let mut out = if names.iter().any(|n| n.eq_ignore_ascii_case("Default")) { Self::default_set() } else { Self::none() };
        for name in names {
            if name.eq_ignore_ascii_case("Default") || name.eq_ignore_ascii_case("None") {
                continue;
            }
            let o = lookup(name).ok_or_else(|| format!("unknown data compare option '{name}'"))?;
            out = out.with(o);
        }
        Ok(out)
    }
}

/// `/options` resolved the way SQL Data Compare's command line does it.
pub struct Resolved {
    pub options: DataOptions,
    /// `Warning:` lines for unknown and obsolete names.
    pub warnings: Vec<String>,
}

impl DataOptions {
    /// `list` is the `/options` value (`None` when not given); `base` is what an absent list or
    /// a bare `Default` gives, and a list with `Default` starts from (the defaults without a
    /// project, nothing with one). A list without `Default` starts from the comment header
    /// only. Unknown names only warn. `/casesensitive` sets CaseSensitiveObjectDefinition
    /// unless `IgnoreCase` is listed.
    pub fn resolve(list: Option<&str>, base: DataOptions, case_sensitive_switch: bool) -> Resolved {
        let names: Vec<&str> = list.unwrap_or_default().split([',', '|']).map(str::trim).filter(|s| !s.is_empty()).collect();
        let mut warnings = Vec::new();
        let (mut ignore_case, mut case_sensitive) = (false, false);
        let bare_default = names.len() == 1 && names[0].eq_ignore_ascii_case("Default");
        let mut out = if names.is_empty() || names.iter().any(|n| n.eq_ignore_ascii_case("Default")) {
            base
        } else {
            DataOptions::none().with(DataOpt::OutputCommentHeader)
        };
        if !bare_default {
            for name in &names {
                let lower = name.to_ascii_lowercase();
                match lower.as_str() {
                    "ignorecase" => ignore_case = true,
                    "casesensitiveobjectdefinition" => case_sensitive = true,
                    "donotoutputcommentheader" => out = out.without(DataOpt::OutputCommentHeader),
                    "default" | "none" => {}
                    "forcecheck" => warnings.push(
                        "The ForceCheck option is obsolete and will be ignored.  SQL Data Compare now restores foreign keys to their original state by default.  ".into(),
                    ),
                    _ => match lookup(name) {
                        Some(o) => out = out.with(o),
                        None => warnings.push(format!("Unrecognised SQL Data Compare option: {name}.")),
                    },
                }
            }
        }
        if case_sensitive || (case_sensitive_switch && !ignore_case) {
            out = out.with(DataOpt::CaseSensitiveObjectDefinition);
        }
        Resolved { options: out, warnings }
    }

    /// The flags of `other` added to these.
    pub fn union(&self, other: &DataOptions) -> DataOptions {
        other.iter().fold(self.clone(), |o, x| o.with(x))
    }
}

fn lookup(name: &str) -> Option<DataOpt> {
    ALL.iter()
        .copied()
        .find(|o| o.name().eq_ignore_ascii_case(name))
        .or_else(|| ALIASES.iter().find(|(a, _)| a.eq_ignore_ascii_case(name)).map(|(_, o)| *o))
}

impl fmt::Display for DataOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.0.iter().map(|o| o.name()).collect();
        write!(f, "{}", if names.is_empty() { "None".to_string() } else { names.join(",") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_tab() {
        assert_eq!(ALL.len(), 26);
        let d = DataOptions::default_set();
        let on: Vec<&str> = d.iter().map(DataOpt::name).collect();
        assert_eq!(on, ["IgnoreSpaces", "IncludeIdentities", "IncludeTimestamps", "DisableKeys", "OutputCommentHeader"]);
        let o = DataOptions::parse("Default,ForceBinaryCollation,CompareUsingChecksum").unwrap();
        assert!(o.has(DataOpt::ForceBinaryCollation) && o.has(DataOpt::UseChecksumComparison) && o.has(DataOpt::DisableKeys));
        assert!(!DataOptions::parse("TrimTrailingSpaces").unwrap().has(DataOpt::DisableKeys));
        let o = DataOptions::none().with(DataOpt::UseChecksumComparison).with(DataOpt::IncludeIdenticalRecords);
        assert!(!o.has(DataOpt::UseChecksumComparison));
        assert!(DataOptions::parse("Nope").is_err());
    }

    #[test]
    fn command_line_resolution() {
        let d = DataOptions::default_set();
        assert_eq!(DataOptions::resolve(None, d.clone(), false).options, d);
        assert_eq!(DataOptions::resolve(Some("default"), d.clone(), false).options, d);
        let r = DataOptions::resolve(Some("IncludeIndexedViews,Nope,DisableAndReenableDDLTriggers"), d.clone(), false);
        let on: Vec<&str> = r.options.iter().map(DataOpt::name).collect();
        assert_eq!(on, ["IncludeViews", "DisableDdlTriggers", "OutputCommentHeader"]);
        assert_eq!(r.warnings, ["Unrecognised SQL Data Compare option: Nope."]);
        let r = DataOptions::resolve(Some("Default,DoNotOutputCommentHeader"), d.clone(), true);
        assert!(!r.options.has(DataOpt::OutputCommentHeader) && r.options.has(DataOpt::CaseSensitiveObjectDefinition));
        assert!(!DataOptions::resolve(Some("IgnoreCase"), d.clone(), true).options.has(DataOpt::CaseSensitiveObjectDefinition));
        // With a project the list is added to nothing; a bare Default adds nothing.
        assert_eq!(DataOptions::resolve(Some("Default"), DataOptions::none(), false).options, DataOptions::none());
    }
}
