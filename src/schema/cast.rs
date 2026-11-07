//! Whether a column can change type with ALTER COLUMN, following SQL Server's conversion rules:
//! implicit conversions keep the data in place; explicit ones need the data copied with CAST
//! (a table rebuild); some types cannot be converted at all.

use crate::catalog::DataType;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cast {
    /// ALTER COLUMN keeps every value.
    Implicit,
    /// ALTER COLUMN works but values can lose precision or fail to fit.
    Lossy,
    /// Needs CAST: rebuild the table and copy the data.
    Explicit,
    /// SQL Server cannot convert the values.
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Family {
    Exact(u8),
    Decimal,
    Money(u8),
    Approx(u8),
    Bit,
    Char,
    Binary,
    Date,
    Time,
    DateTime(u8),
    Guid,
    Xml,
    Variant,
    LegacyText,
    Image,
    Timestamp,
    Other,
}

fn family(name: &str) -> Family {
    match name {
        "tinyint" => Family::Exact(1),
        "smallint" => Family::Exact(2),
        "int" => Family::Exact(4),
        "bigint" => Family::Exact(8),
        "decimal" | "numeric" => Family::Decimal,
        "smallmoney" => Family::Money(4),
        "money" => Family::Money(8),
        "real" => Family::Approx(4),
        "float" => Family::Approx(8),
        "bit" => Family::Bit,
        "char" | "varchar" | "nchar" | "nvarchar" | "sysname" => Family::Char,
        "binary" | "varbinary" => Family::Binary,
        "date" => Family::Date,
        "time" => Family::Time,
        "smalldatetime" => Family::DateTime(1),
        "datetime" => Family::DateTime(2),
        "datetime2" => Family::DateTime(3),
        "datetimeoffset" => Family::DateTime(4),
        "uniqueidentifier" => Family::Guid,
        "xml" => Family::Xml,
        "sql_variant" => Family::Variant,
        "text" | "ntext" => Family::LegacyText,
        "image" => Family::Image,
        "timestamp" | "rowversion" => Family::Timestamp,
        _ => Family::Other,
    }
}

/// How a column of type `old` (the target) converts to `new` (the source).
pub fn classify(old: &DataType, new: &DataType) -> Cast {
    use Family::*;
    // User-defined and CLR types only stay put when unchanged.
    if old.schema.is_some() || new.schema.is_some() {
        return if old.schema == new.schema && old.name.eq_ignore_ascii_case(&new.name) { Cast::Implicit } else { Cast::Explicit };
    }
    let (o, n) = (family(&old.name), family(&new.name));
    if old.name == new.name {
        return match o {
            Decimal => decimal(old, new),
            Xml => match (&old.xml_collection, &new.xml_collection) {
                (_, None) => Cast::Implicit,
                (None, Some(_)) => Cast::Lossy,
                (Some(a), Some(b)) if a == b => Cast::Implicit,
                _ => Cast::Explicit,
            },
            _ => Cast::Implicit,
        };
    }
    match (o, n) {
        (Timestamp, _) | (_, Timestamp) => Cast::Explicit,
        (Other, _) | (_, Other) => Cast::None,
        (Decimal, Decimal) => decimal(old, new),
        // Numbers: widening is safe, narrowing may overflow or round.
        (Exact(a), Exact(b)) => if b >= a { Cast::Implicit } else { Cast::Lossy },
        (Exact(_) | Bit, Decimal | Money(_) | Approx(_)) => Cast::Implicit,
        (Bit, Exact(_)) => Cast::Implicit,
        (Exact(_), Bit) => Cast::Lossy,
        (Decimal | Money(_), Exact(_) | Approx(_) | Bit) => Cast::Lossy,
        (Decimal, Money(_)) | (Money(_), Decimal) => Cast::Implicit,
        (Money(a), Money(b)) => if b >= a { Cast::Implicit } else { Cast::Lossy },
        (Approx(a), Approx(b)) => if b >= a { Cast::Implicit } else { Cast::Lossy },
        (Approx(_), Exact(_) | Bit | Money(_) | Decimal) => Cast::Lossy,
        // Anything readable as text converts to text; text converts back when the values parse.
        (Exact(_) | Decimal | Money(_) | Approx(_) | Bit | Date | Time | DateTime(_) | Guid, Char) => Cast::Implicit,
        (Char, Exact(_) | Decimal | Money(_) | Approx(_) | Bit | Date | Time | DateTime(_) | Guid | Xml) => Cast::Implicit,
        // Between the character types (varchar to nvarchar…) and between the binary ones.
        (Char, Char) | (Binary, Binary) => Cast::Implicit,
        (Char | LegacyText, LegacyText) | (LegacyText, Char) => Cast::Implicit,
        (Binary, Char | Exact(_) | Decimal | Money(_) | Bit | Guid | Image | Xml) => Cast::Implicit,
        (Exact(_) | Decimal | Money(_) | Bit | Guid | Image, Binary) => Cast::Implicit,
        (Char, Binary) => Cast::Explicit,
        // Dates.
        (DateTime(a), DateTime(b)) => if b > a || (a == 1 && b > 1) { Cast::Implicit } else { Cast::Lossy },
        (Date, DateTime(_)) => Cast::Implicit,
        (Time, DateTime(b)) if b >= 2 => Cast::Implicit,
        (DateTime(_), Date | Time) => Cast::Lossy,
        (DateTime(_), Exact(_) | Decimal | Money(_) | Approx(_) | Bit) => Cast::Explicit,
        // sql_variant holds most base types but does not hand them back implicitly.
        (_, Variant) if !matches!(o, LegacyText | Image | Xml) => Cast::Implicit,
        (Variant, _) => Cast::Explicit,
        (Xml, Char | Binary) => Cast::Explicit,
        _ => Cast::None,
    }
}

fn decimal(old: &DataType, new: &DataType) -> Cast {
    let (op, os) = (old.precision.unwrap_or(18) as i32, old.scale.unwrap_or(0) as i32);
    let (np, ns) = (new.precision.unwrap_or(18) as i32, new.scale.unwrap_or(0) as i32);
    if np - ns >= op - os && ns >= os { Cast::Implicit } else { Cast::Lossy }
}

/// Characters or bytes a value can hold; `None` when the type has no length.
fn capacity(t: &DataType) -> Option<i64> {
    let len = t.length?;
    Some(if len < 0 { i64::MAX } else { len as i64 })
}

/// Whether a type change shortens values (length or fractional seconds) and whether it drops
/// Unicode (n-types to single-byte ones).
pub fn losses(old: &DataType, new: &DataType) -> (bool, bool) {
    let shorter = matches!((capacity(old), capacity(new)), (Some(o), Some(n)) if n < o)
        || (matches!(new.name.as_str(), "time" | "datetime2" | "datetimeoffset")
            && old.name == new.name
            && new.scale.unwrap_or(7) < old.scale.unwrap_or(7));
    let unicode = matches!(old.name.as_str(), "nchar" | "nvarchar" | "ntext") && matches!(new.name.as_str(), "char" | "varchar" | "text");
    (shorter, unicode)
}

/// Warnings about data that may not survive a type change (length, Unicode, fractional seconds).
pub fn data_loss(old: &DataType, new: &DataType) -> Vec<String> {
    let mut out = Vec::new();
    if let (Some(o), Some(n)) = (capacity(old), capacity(new))
        && n < o
    {
        out.push(format!("values longer than {} will be truncated ({} → {})", new.length.unwrap_or(0), old.sql(), new.sql()));
    }
    if matches!(old.name.as_str(), "nchar" | "nvarchar" | "ntext") && matches!(new.name.as_str(), "char" | "varchar" | "text") {
        out.push(format!("Unicode characters may be lost ({} → {})", old.sql(), new.sql()));
    }
    if matches!(new.name.as_str(), "time" | "datetime2" | "datetimeoffset")
        && old.name == new.name
        && new.scale.unwrap_or(7) < old.scale.unwrap_or(7)
    {
        out.push(format!("fractional seconds will be rounded ({} → {})", old.sql(), new.sql()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str) -> DataType {
        DataType { name: name.into(), ..Default::default() }
    }

    fn len(name: &str, l: i32) -> DataType {
        DataType { name: name.into(), length: Some(l), ..Default::default() }
    }

    #[test]
    fn classes() {
        assert_eq!(classify(&len("varchar", 50), &len("varchar", 100)), Cast::Implicit);
        assert_eq!(classify(&len("varchar", 100), &len("varchar", 50)), Cast::Implicit);
        assert_eq!(classify(&t("int"), &t("bigint")), Cast::Implicit);
        assert_eq!(classify(&t("bigint"), &t("int")), Cast::Lossy);
        assert_eq!(classify(&len("varchar", 20), &len("nvarchar", 30)), Cast::Implicit);
        assert_eq!(classify(&len("nchar", 20), &len("varchar", 30)), Cast::Implicit);
        assert_eq!(classify(&len("binary", 8), &len("varbinary", 30)), Cast::Implicit);
        assert_eq!(classify(&t("int"), &len("varchar", 10)), Cast::Implicit);
        assert_eq!(classify(&len("varchar", 10), &t("int")), Cast::Implicit);
        assert_eq!(classify(&t("datetime"), &t("int")), Cast::Explicit);
        assert_eq!(classify(&t("date"), &t("datetime2")), Cast::Implicit);
        assert_eq!(classify(&t("datetime2"), &t("date")), Cast::Lossy);
        assert_eq!(classify(&t("uniqueidentifier"), &t("int")), Cast::None);
        assert_eq!(classify(&t("timestamp"), &len("binary", 8)), Cast::Explicit);
        let d = |p, s| DataType { name: "decimal".into(), precision: Some(p), scale: Some(s), ..Default::default() };
        assert_eq!(classify(&d(10, 2), &d(12, 2)), Cast::Implicit);
        assert_eq!(classify(&d(12, 2), &d(10, 2)), Cast::Lossy);
    }

    #[test]
    fn losses() {
        assert_eq!(data_loss(&len("nvarchar", 100), &len("varchar", 50)).len(), 2);
        assert!(data_loss(&len("varchar", 50), &len("varchar", -1)).is_empty());
        assert_eq!(data_loss(&len("varchar", -1), &len("varchar", 4000)).len(), 1);
    }
}
