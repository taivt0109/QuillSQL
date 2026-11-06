//! The snapshot records of the objects of `catalog::objects` (members 3, 4 and 17–45): read
//! into the model instead of stepped over.

use super::*;
use crate::sources::events::event_name;

/// What the members of these kinds hold, until the blobs and permissions can be attached.
#[derive(Default)]
pub(super) struct More {
    pub(super) defaults: Vec<BindableObject>,
    pub(super) rules: Vec<BindableObject>,
    pub(super) assemblies: Vec<(Assembly, Vec<u64>, Vec<RawPerm>)>,
    pub(super) assembly_blobs: HashMap<u64, Vec<u8>>,
    pub(super) xml_blobs: HashMap<u64, Vec<u8>>,
    pub(super) xml_collections: Vec<(XmlSchemaCollection, i32, Vec<RawPerm>)>,
    pub(super) message_types: Vec<(MessageType, Vec<RawPerm>)>,
    pub(super) contracts: Vec<(Contract, Vec<RawPerm>)>,
    pub(super) queues: Vec<(Queue, Vec<RawPerm>)>,
    pub(super) services: Vec<(Service, Vec<RawPerm>)>,
    pub(super) routes: Vec<(Route, Vec<RawPerm>)>,
    pub(super) event_notifications: Vec<EventNotification>,
    pub(super) partition_functions: Vec<PartitionFunction>,
    pub(super) partition_schemes: Vec<PartitionScheme>,
    pub(super) ddl_triggers: Vec<(DdlTrigger, i32)>,
    pub(super) bindings: Vec<(ServiceBinding, Vec<RawPerm>)>,
    pub(super) certificates: Vec<(Certificate, Vec<RawPerm>)>,
    pub(super) symmetric_keys: Vec<(SymmetricKey, Vec<RawPerm>)>,
    pub(super) asymmetric_keys: Vec<(AsymmetricKey, Vec<RawPerm>)>,
    pub(super) fulltext_catalogs: Vec<(FullTextCatalog, Vec<RawPerm>)>,
    pub(super) stoplists: Vec<FullTextStoplist>,
    pub(super) property_lists: Vec<SearchPropertyList>,
    pub(super) policies: Vec<(SecurityPolicy, Vec<RawPerm>)>,
    pub(super) file_formats: Vec<ExternalFileFormat>,
    pub(super) data_sources: Vec<ExternalDataSource>,
    pub(super) external_tables: Vec<(ExternalTable, Vec<RawPerm>)>,
    pub(super) credentials: Vec<String>,
}

/// `[QuillClr]` or `QuillClr` → `[QuillClr]`.
pub(super) fn bracket(s: &str) -> String {
    quote(&unquote(s))
}

/// `yyyy-MM-dd HH:mm:ss` of raw .NET ticks (no kind bits).
pub(super) fn ticks_text(ticks: i64) -> String {
    const UNIX: i64 = 621_355_968_000_000_000;
    let secs = (ticks - UNIX).div_euclid(10_000_000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Raw .NET ticks of `yyyy-MM-dd HH:mm:ss`.
pub(super) fn text_ticks(s: &str) -> i64 {
    let n: Vec<i64> = s.split(|c: char| !c.is_ascii_digit()).filter(|x| !x.is_empty()).filter_map(|x| x.parse().ok()).collect();
    let (y, m, d) = (n.first().copied().unwrap_or(1), n.get(1).copied().unwrap_or(1), n.get(2).copied().unwrap_or(1));
    let (hh, mm, ss) = (n.get(3).copied().unwrap_or(0), n.get(4).copied().unwrap_or(0), n.get(5).copied().unwrap_or(0));
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    (days * 86_400 + hh * 3600 + mm * 60 + ss) * 10_000_000 + 621_355_968_000_000_000
}

pub(super) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

/// Message type validation codes.
pub(super) fn validation(code: i32) -> &'static str {
    match code {
        1 => "WELL_FORMED_XML",
        2 => "EMPTY",
        _ => "NONE",
    }
}

pub(super) fn private_key(code: i32) -> &'static str {
    match code {
        1 => "MK",
        2 => "PW",
        3 => "SK",
        _ => "NA",
    }
}

pub(super) const SYMMETRIC_ALGORITHMS: [&str; 8] = ["RC2", "RC4", "DES", "TRIPLE_DES", "DESX", "AES_128", "AES_192", "AES_256"];
pub(super) const ASYMMETRIC_ALGORITHMS: [&str; 5] = ["RSA_512", "RSA_1024", "RSA_2048", "RSA_3072", "RSA_4096"];
pub(super) const DATA_SOURCE_TYPES: [&str; 7] = ["HADOOP", "RDBMS", "SHARD_MAP_MANAGER", "REMOTE_DATA_ARCHIVE", "", "BLOB_STORAGE", ""];
pub(super) const BLOCK_OPERATIONS: [&str; 5] = ["", "AFTER INSERT", "AFTER UPDATE", "BEFORE UPDATE", "BEFORE DELETE"];

impl Cur<'_> {
    pub(super) fn bindable(&mut self, rule: bool) -> R<BindableObject> {
        let v = self.f32()?;
        let name = self.s()?;
        let schema = self.s()?;
        if (!rule && v > 1.0) || (rule && v >= 7.0) {
            self.i32()?;
        }
        let definition = self.s()?;
        self.eprops()?;
        if v >= 11.45 {
            self.ss()?;
        }
        Ok(BindableObject { schema, name, definition })
    }

    /// A parameter as the model writes it, and its input type.
    pub(super) fn param_full(&mut self) -> R<(Parameter, DataType)> {
        let v = self.f32()?;
        self.i32()?;
        let name = self.s()?;
        let mut p = Parameter { name, ..Default::default() };
        let mut ty = DataType::default();
        if v >= 4.0 {
            let (size, scale, precision) = (self.i32()?, self.i32()?, self.i32()?);
            let type_name = self.s()?;
            let type_schema = if v >= 6.0 { self.s()? } else { "sys".into() };
            let has_default = self.bool()?;
            let default = self.str()?;
            p.output = self.bool()?;
            let document = self.bool()?;
            if v < 13.7912 {
                self.str()?;
            }
            let xml = self.str()?.filter(|x| !x.is_empty());
            ty = if is_user_schema(&type_schema) {
                DataType { name: type_name, schema: Some(type_schema), ..Default::default() }
            } else {
                let mut t = system_type(&type_name, size, precision, scale);
                if t.name == "xml" {
                    t.xml_collection = xml;
                    t.xml_document = document;
                }
                t
            };
            p.data_type = ty.sql();
            p.default = default.filter(|_| has_default);
        }
        if v >= 13.63 {
            self.str()?;
        }
        Ok((p, ty))
    }

    /// Parameters, then the id map.
    pub(super) fn params_full(&mut self) -> R<Vec<(Parameter, DataType)>> {
        let list = self.coll(Cur::param_full)?;
        for _ in 0..list.len() {
            self.i32()?;
            self.str()?;
        }
        Ok(list)
    }

    pub(super) fn assembly_full(&mut self) -> R<(Assembly, Vec<u64>, Vec<RawPerm>)> {
        let versioned = self.versioned_record()?;
        let mut a = Assembly { name: self.s()?, owner: self.str()?, ..Default::default() };
        self.i32()?;
        self.bool()?;
        self.eprops()?;
        self.take(16)?;
        a.clr_name = self.s()?;
        a.visible = self.bool()?;
        let n = self.count()?;
        let mut blobs = Vec::new();
        for _ in 0..n {
            let file_versioned = self.versioned_record()?;
            let name = self.s()?;
            blobs.push(self.u64()?);
            self.i32()?;
            if file_versioned {
                self.ss()?;
            }
            a.files.push(AssemblyFile { name, content: String::new() });
        }
        a.permission_set = match self.i32()? {
            2 => "EXTERNAL_ACCESS",
            3 => "UNSAFE",
            _ => "SAFE",
        }
        .into();
        let perms = self.perms()?;
        if versioned {
            self.sss(3)?;
        }
        Ok((a, blobs, perms))
    }

    pub(super) fn xml_collection_full(&mut self) -> R<(XmlSchemaCollection, i32, Vec<RawPerm>)> {
        let v = self.f32()?;
        let mut x = XmlSchemaCollection { name: self.s()?, schema: self.s()?, ..Default::default() };
        let id = self.i32()?;
        self.bool()?;
        self.eprops()?;
        let perms = self.perms()?;
        if v <= 13.65 {
            let n = self.count()?;
            for _ in 0..n {
                self.str()?;
                self.str()?;
            }
            for _ in 0..3 {
                self.strs()?;
            }
        }
        if v >= 7.0 {
            x.owner = self.str()?.filter(|o| !o.is_empty());
        }
        self.two_ss(v)?;
        Ok((x, id, perms))
    }

    pub(super) fn message_type_full(&mut self) -> R<Option<(MessageType, Vec<RawPerm>)>> {
        let v = self.f32()?;
        let mut m = MessageType { name: self.s()?, owner: self.str()?, ..Default::default() };
        let id = self.i32()?;
        self.bool()?;
        let code = self.i32()?;
        m.xml_collection = self.str()?.filter(|x| !x.is_empty());
        m.validation = if m.xml_collection.is_some() { "VALID_XML" } else { validation(code) }.into();
        self.eprops()?;
        let perms = self.perms()?;
        let n = self.count_or_none()?;
        for _ in 0..n {
            self.str()?;
        }
        self.two_ss(v)?;
        Ok((id > 65535).then_some((m, perms)))
    }

    /// A contract; None for the system ones.
    pub(super) fn contract_full(&mut self) -> R<Option<(Contract, Vec<RawPerm>)>> {
        let v = self.f32()?;
        let mut k = Contract { name: self.s()?, owner: self.str()?, ..Default::default() };
        self.i32()?;
        self.eprops()?;
        let system = self.bool()?;
        self.bool()?;
        k.messages = self.coll(|c| {
            let message_type = unquote(&c.s()?);
            let (i, t) = (c.bool()?, c.bool()?);
            let sent_by = match (i, t) {
                (true, true) => "ANY",
                (true, false) => "INITIATOR",
                _ => "TARGET",
            };
            Ok(ContractMessage { message_type, sent_by: sent_by.into() })
        })?;
        k.messages.sort_by(|a, b| a.message_type.cmp(&b.message_type));
        let perms = self.perms()?;
        let n = self.count_or_none()?;
        for _ in 0..n {
            self.str()?;
        }
        self.two_ss(v)?;
        Ok((!system && k.name != "DEFAULT").then_some((k, perms)))
    }

    pub(super) fn event_notification_full(&mut self) -> R<EventNotification> {
        let v = self.f32()?;
        let mut e = EventNotification { name: self.s()?, ..Default::default() };
        self.str()?;
        self.i32()?;
        self.bool()?;
        e.service = self.s()?;
        e.broker_instance = self.s()?;
        e.fan_in = self.bool()?;
        e.events = self.coll(Cur::i32)?.into_iter().map(|c| event_name(c).map(str::to_string).unwrap_or_else(|| c.to_string())).collect();
        e.events.sort();
        self.eprops()?;
        if v < 7.0 {
            self.perms()?;
        }
        if v >= 11.45 {
            self.ss()?;
        }
        Ok(e)
    }

    pub(super) fn queue_full(&mut self) -> R<Option<(Queue, Vec<RawPerm>)>> {
        let v = self.f32()?;
        let mut q = Queue { name: self.s()?, schema: self.s()?, ..Default::default() };
        self.i32()?;
        let system = self.bool()?;
        self.bool()?;
        let max_readers = i32::from(self.i16()?);
        let procedure = self.str()?.filter(|p| !p.is_empty());
        let execute_as = if v >= 7.2 {
            self.execute_as()?
        } else {
            let who = self.str()?;
            self.bool()?;
            who
        };
        let activation = self.bool()?;
        self.bool()?; // receive
        q.enabled = self.bool()?;
        q.retention = self.bool()?;
        q.filegroup = self.str()?.filter(|f| !f.is_empty());
        self.eprops()?;
        q.event_notifications = self.coll(Cur::event_notification_full)?;
        let queue = ObjectName::new(&q.schema, &q.name);
        for e in &mut q.event_notifications {
            e.queue = Some(queue.clone());
        }
        q.event_notifications.sort_by(|a, b| a.name.cmp(&b.name));
        self.strs()?;
        if v >= 7.0 {
            q.owner = self.str()?.filter(|o| !o.is_empty());
        }
        let perms = if v >= 7.1 { self.perms()? } else { Vec::new() };
        if v >= 11.2 {
            q.poison_message_handling = self.bool()?;
        }
        self.two_ss(v)?;
        q.activation = procedure.map(|p| QueueActivation {
            enabled: activation,
            procedure: two_part(&p).map(|n| n.quoted()).unwrap_or(p),
            max_readers,
            // SELF is stored as the user that made the queue.
            execute_as: execute_as.map(|w| if w == "dbo" { "SELF".into() } else { w }),
        });
        Ok((!system).then_some((q, perms)))
    }

    pub(super) fn service_full(&mut self) -> R<Option<(Service, Vec<RawPerm>)>> {
        let v = self.f32()?;
        let mut s = Service { name: self.s()?, owner: self.str()?, ..Default::default() };
        self.i32()?;
        let system = self.bool()?;
        self.bool()?;
        let queue = self.s()?;
        s.queue = two_part(&queue).unwrap_or_default();
        s.contracts = self.coll(Cur::contract_any)?;
        s.contracts.sort();
        self.eprops()?;
        let perms = self.perms()?;
        if v >= 4.1 {
            self.coll(Cur::event_notification_full)?;
        }
        self.two_ss(v)?;
        Ok((!system).then_some((s, perms)))
    }

    /// A contract record nested in a service: its name.
    fn contract_any(&mut self) -> R<String> {
        let v = self.f32()?;
        let name = self.s()?;
        self.str()?;
        self.i32()?;
        self.eprops()?;
        self.bool()?;
        self.bool()?;
        self.coll(|c| {
            c.s()?;
            c.bool()?;
            c.bool()
        })?;
        self.perms()?;
        let n = self.count_or_none()?;
        for _ in 0..n {
            self.str()?;
        }
        self.two_ss(v)?;
        Ok(name)
    }

    pub(super) fn route_full(&mut self) -> R<Option<(Route, Vec<RawPerm>)>> {
        let v = self.f32()?;
        let mut r = Route { name: self.s()?, owner: self.str()?, ..Default::default() };
        self.i32()?;
        self.bool()?;
        let system = self.bool()?;
        r.remote_service = self.str()?.filter(|x| !x.is_empty());
        r.broker_instance = self.str()?.filter(|x| !x.is_empty());
        r.address = self.s()?;
        r.mirror_address = self.str()?.filter(|x| !x.is_empty());
        let lifetime = self.i64()?;
        if self.bool()? {
            r.expires = Some(format!("{}.000", ticks_text(lifetime)));
        }
        self.eprops()?;
        let perms = self.perms()?;
        self.two_ss(v)?;
        Ok((!system && r.name != "AutoCreatedLocal").then_some((r, perms)))
    }

    pub(super) fn partition_function_full(&mut self) -> R<PartitionFunction> {
        let v = self.f32()?;
        let mut f = PartitionFunction { name: self.s()?, ..Default::default() };
        self.str()?;
        self.i32()?;
        self.bool()?;
        f.range_right = self.bool()?;
        let params = self.params_full()?;
        f.data_type = params.into_iter().next().map(|p| p.1).unwrap_or_default();
        self.eprops()?;
        if v < 7.0 {
            self.perms()?;
        }
        if v < 13.78 {
            self.strs()?;
        }
        let n = self.count()?;
        let quoted = matches!(
            f.data_type.name.as_str(),
            "char" | "varchar" | "nchar" | "nvarchar" | "date" | "datetime" | "smalldatetime" | "datetime2" | "time" | "datetimeoffset"
        );
        for _ in 0..n {
            let null = v >= 11.0 && self.bool()?;
            let value = if null { None } else { self.str()? };
            f.boundaries.push(match value {
                None => "NULL".into(),
                Some(x) if quoted => format!("N'{}'", x.replace('\'', "''")),
                Some(x) if f.data_type.name == "uniqueidentifier" => format!("'{{{}}}'", x.trim_matches(['{', '}'])),
                Some(x) => x,
            });
        }
        if v >= 11.45 {
            self.ss()?;
        }
        Ok(f)
    }

    pub(super) fn partition_scheme_full(&mut self) -> R<PartitionScheme> {
        let v = self.f32()?;
        let mut s = PartitionScheme { name: self.s()?, ..Default::default() };
        self.str()?;
        self.i32()?;
        s.function = unquote(&self.s()?);
        self.eprops()?;
        if v < 7.0 {
            self.perms()?;
        }
        self.f32()?;
        let n = self.count()?;
        for _ in 0..n {
            self.f32()?;
            s.filegroups.push(self.s()?);
        }
        if v >= 11.45 {
            self.ss()?;
        }
        Ok(s)
    }

    pub(super) fn ddl_trigger_full(&mut self) -> R<(DdlTrigger, i32)> {
        let v = self.f32()?;
        let mut t = DdlTrigger { name: self.s()?, ..Default::default() };
        self.str()?;
        self.i32()?;
        self.bool()?;
        t.disabled = self.bool()?;
        let clr = self.bool()?;
        t.settings.quoted_identifier = self.bool()?;
        t.settings.ansi_nulls = self.bool()?;
        let text_id = self.i32()?;
        if v >= 7.0 {
            t.events = self.coll(Cur::i32)?.into_iter().map(|c| event_name(c).map(str::to_string).unwrap_or_else(|| c.to_string())).collect();
        } else {
            let n = self.count_or_none()?;
            for _ in 0..n {
                t.events.push(self.s()?);
            }
        }
        t.events.sort();
        let binding = [self.s()?, self.s()?, self.s()?];
        let execute_as = if v >= 7.1 { self.execute_as()? } else { self.str()? };
        self.eprops()?;
        t.encrypted = self.bool()?;
        if v >= 11.45 {
            self.ss()?;
        }
        if clr {
            t.clr_method = Some(binding.iter().map(|b| bracket(b)).collect::<Vec<_>>().join("."));
            t.execute_as = execute_as;
        }
        Ok((t, text_id))
    }

    pub(super) fn service_binding_full(&mut self) -> R<(ServiceBinding, Vec<RawPerm>)> {
        let v = self.f32()?;
        let mut b = ServiceBinding { name: self.s()?, owner: self.str()?, ..Default::default() };
        self.i32()?;
        self.bool()?;
        b.remote_service = self.s()?;
        b.user = self.s()?;
        b.anonymous = self.bool()?;
        self.eprops()?;
        let perms = self.perms()?;
        self.two_ss(v)?;
        Ok((b, perms))
    }

    pub(super) fn certificate_full(&mut self) -> R<(Certificate, Vec<RawPerm>)> {
        let v = self.f32()?;
        let mut c = Certificate { name: self.s()?, owner: self.str()?, ..Default::default() };
        c.private_key = private_key(self.i32()?).into();
        c.active_for_dialog = self.bool()?;
        c.issuer = self.s()?;
        c.subject = self.s()?;
        c.start_date = ticks_text(self.i64()?);
        c.expiry_date = ticks_text(self.i64()?);
        self.str()?;
        let n = self.count()?;
        c.thumbprint = format!("0x{}", hex(self.take(n)?));
        let perms = self.perms()?;
        self.two_ss(v)?;
        Ok((c, perms))
    }

    pub(super) fn symmetric_key_full(&mut self) -> R<Option<(SymmetricKey, Vec<RawPerm>)>> {
        let v = self.f32()?;
        let mut k = SymmetricKey { name: self.s()?, owner: self.str()?, ..Default::default() };
        k.modify_date = ticks_text(self.i64()?);
        k.create_date = ticks_text(self.i64()?);
        k.key_guid = guid(self.bytes()?).trim_matches(['{', '}']).to_uppercase();
        k.key_length = self.i32()?;
        let code = self.i32()? - 1;
        k.algorithm = usize::try_from(code).ok().and_then(|i| SYMMETRIC_ALGORITHMS.get(i)).copied().unwrap_or_default().into();
        let perms = self.perms()?;
        self.two_ss(v)?;
        Ok((!k.name.starts_with("##")).then_some((k, perms)))
    }

    pub(super) fn asymmetric_key_full(&mut self) -> R<(AsymmetricKey, Vec<RawPerm>)> {
        let v = self.f32()?;
        let mut k = AsymmetricKey { name: self.s()?, owner: self.str()?, ..Default::default() };
        let n = self.count()?;
        k.thumbprint = format!("0x{}", hex(self.take(n)?));
        k.key_length = self.i32()?;
        let (code, pk) = (self.i32()?, self.i32()?);
        k.algorithm = usize::try_from(code).ok().and_then(|i| ASYMMETRIC_ALGORITHMS.get(i)).copied().unwrap_or_default().into();
        k.private_key = private_key(pk).into();
        let perms = self.perms()?;
        self.two_ss(v)?;
        Ok((k, perms))
    }

    pub(super) fn fulltext_catalog_full(&mut self) -> R<(FullTextCatalog, Vec<RawPerm>)> {
        let v = self.f32()?;
        let mut f = FullTextCatalog { name: self.s()?, ..Default::default() };
        self.bool()?;
        if v >= 4.0 {
            f.owner = self.str()?.filter(|o| !o.is_empty());
            if v <= 14.0 {
                self.str()?;
            }
            f.is_default = self.bool()?;
            if v <= 14.0 {
                self.str()?;
            }
            f.accent_sensitive = self.bool()?;
        }
        self.strs()?;
        let perms = if v >= 4.0 { self.perms()? } else { Vec::new() };
        self.two_ss(v)?;
        Ok((f, perms))
    }

    fn words(&mut self, out: &mut Vec<Stopword>) -> R<()> {
        let n = self.count()?;
        for _ in 0..n {
            let language = self.i32()?;
            for word in self.strs()? {
                out.push(Stopword { language, word });
            }
        }
        Ok(())
    }

    /// Stoplists based on the system list keep only their changes to it; those read as the
    /// added words.
    pub(super) fn stoplist_full(&mut self) -> R<FullTextStoplist> {
        let v = self.f32()?;
        let mut l = FullTextStoplist { name: self.s()?, owner: self.str()?.filter(|o| !o.is_empty()), ..Default::default() };
        self.bool()?;
        self.f32()?;
        let from_system = self.bool()?;
        self.words(&mut l.words)?;
        if from_system {
            let mut removed = Vec::new();
            self.words(&mut removed)?;
        }
        l.words.sort();
        self.two_ss(v)?;
        Ok(l)
    }

    pub(super) fn property_list_full(&mut self) -> R<SearchPropertyList> {
        self.perms()?;
        let v = self.f32()?;
        let mut l = SearchPropertyList { name: self.s()?, owner: self.str()?.filter(|o| !o.is_empty()), ..Default::default() };
        let n = self.count()?;
        for _ in 0..n {
            self.f32()?;
            let name = self.s()?;
            let set_guid = self.s()?.trim_matches(['{', '}']).to_uppercase();
            let int_id = self.i32()?;
            let description = self.str()?.filter(|d| !d.is_empty());
            self.i32()?;
            l.properties.push(SearchProperty { name, set_guid, int_id, description });
        }
        l.properties.sort_by(|a, b| a.name.cmp(&b.name));
        self.i32()?;
        self.two_ss(v)?;
        Ok(l)
    }

    pub(super) fn security_policy_full(&mut self) -> R<(SecurityPolicy, Vec<RawPerm>)> {
        let v = self.f32()?;
        self.i32()?;
        let mut p = SecurityPolicy { schema: self.s()?, name: self.s()?, ..Default::default() };
        self.str()?;
        p.enabled = self.bool()?;
        p.schema_bound = if v >= 23.0 { self.bool()? } else { true };
        p.not_for_replication = self.bool()?;
        let n = self.count()?;
        for _ in 0..n {
            let function = self.s()?;
            let arguments = self.s()?;
            let table = self.s()?;
            let (kind, operation) = if v >= 11.36 { (self.i32()?, self.i32()?) } else { (0, 0) };
            p.predicates.push(SecurityPredicate {
                kind: if kind == 1 { "BLOCK" } else { "FILTER" }.into(),
                function: two_part(&function).map(|n| n.quoted()).unwrap_or(function),
                arguments,
                table: two_part(&table).unwrap_or_default(),
                operation: usize::try_from(operation).ok().and_then(|o| BLOCK_OPERATIONS.get(o)).filter(|o| !o.is_empty()).map(|o| o.to_string()),
            });
        }
        self.eprops()?;
        let perms = self.perms()?;
        self.two_ss(v)?;
        Ok((p, perms))
    }

    pub(super) fn file_format_full(&mut self) -> R<ExternalFileFormat> {
        self.i32()?;
        self.str()?;
        let mut f = ExternalFileFormat { name: self.s()?, ..Default::default() };
        let nonempty = |x: Option<String>| x.filter(|s| !s.is_empty());
        f.format_type = self.s()?.to_ascii_uppercase();
        f.field_terminator = nonempty(self.str()?);
        f.string_delimiter = nonempty(self.str()?);
        f.date_format = nonempty(self.str()?);
        f.use_type_default = self.bool()?;
        f.serde_method = nonempty(self.str()?);
        self.str()?;
        f.encoding = nonempty(self.str()?);
        f.data_compression = nonempty(self.str()?);
        if self.u8()? != 0 {
            f.first_row = Some(self.i32()?).filter(|n| *n > 1);
        }
        self.ss()?;
        Ok(f)
    }

    pub(super) fn data_source_full(&mut self) -> R<ExternalDataSource> {
        let nonempty = |x: Option<String>| x.filter(|s| !s.is_empty());
        let mut d = ExternalDataSource { name: self.s()?, location: self.s()?, ..Default::default() };
        d.kind = usize::try_from(self.i32()?).ok().and_then(|i| DATA_SOURCE_TYPES.get(i)).filter(|k| !k.is_empty()).map(|k| k.to_string());
        d.resource_manager_location = nonempty(self.str()?);
        d.credential = nonempty(self.str()?);
        d.database_name = nonempty(self.str()?);
        d.shard_map_name = nonempty(self.str()?);
        d.connection_options = nonempty(self.str()?);
        d.pushdown = Some(self.bool()?);
        self.ss()?;
        Ok(d)
    }

    pub(super) fn external_table_full(&mut self) -> R<(ExternalTable, Vec<RawPerm>)> {
        self.i32()?;
        let mut t = ExternalTable { schema: self.s()?, name: self.s()?, ..Default::default() };
        self.bool()?;
        t.columns = self.coll(Cur::field)?.iter().enumerate().map(|(i, f)| column(f, i + 1)).collect();
        let nonempty = |x: Option<String>| x.filter(|s| !s.is_empty());
        t.data_source = self.s()?;
        t.file_format = nonempty(self.str()?);
        t.location = nonempty(self.str()?);
        let reject_type = self.i32()?;
        let reject_value = if self.u8()? != 0 { Some(self.f64()?) } else { None };
        let reject_sample = if self.u8()? != 0 { Some(self.i32()?) } else { None };
        t.remote_schema = nonempty(self.str()?);
        t.remote_object = nonempty(self.str()?);
        let distribution = if self.u8()? != 0 { Some(self.i32()?) } else { None };
        let sharding = self.str()?;
        self.str()?;
        self.ss()?;
        let perms = self.perms()?;
        self.eprops()?;
        t.reject_type = reject_value.map(|_| if reject_type == 1 { "PERCENTAGE" } else { "VALUE" }.to_string());
        t.reject_value = reject_value.map(|v| v.to_string());
        t.reject_sample_value = reject_sample.map(|v| v.to_string());
        t.distribution = match distribution {
            Some(0) => Some(format!("SHARDED({})", quote(sharding.as_deref().unwrap_or_default()))),
            Some(1) => Some("REPLICATED".into()),
            Some(2) => Some("ROUND_ROBIN".into()),
            _ => None,
        };
        Ok((t, perms))
    }
}

impl More {
    /// Moves everything into the database, with its permissions.
    pub(super) fn build(self, db: &mut Database, permissions: &mut Vec<Permission>, texts: &dyn Fn(i32) -> (String, bool)) {
        let mut grant = |class: &str, schema: Option<&str>, name: &str, perms: &[RawPerm]| {
            for p in perms {
                permissions.extend(grants(p, class, schema, name));
            }
        };
        db.defaults = self.defaults;
        db.rules = self.rules;
        for (mut a, blobs, perms) in self.assemblies {
            for (f, id) in a.files.iter_mut().zip(blobs) {
                f.content = self.assembly_blobs.get(&id).map(|b| hex(b)).unwrap_or_default();
            }
            grant("ASSEMBLY", None, &a.name, &perms);
            db.assemblies.push(a);
        }
        for (mut x, id, perms) in self.xml_collections {
            x.definition = self.xml_blobs.get(&(id as i64 as u64)).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
            grant("XML SCHEMA COLLECTION", Some(&x.schema), &x.name, &perms);
            db.xml_schema_collections.push(x);
        }
        for (m, perms) in self.message_types {
            grant("MESSAGE TYPE", None, &m.name, &perms);
            db.message_types.push(m);
        }
        for (k, perms) in self.contracts {
            grant("CONTRACT", None, &k.name, &perms);
            db.contracts.push(k);
        }
        for (q, perms) in self.queues {
            grant("OBJECT", Some(&q.schema), &q.name, &perms);
            db.queues.push(q);
        }
        for (s, perms) in self.services {
            grant("SERVICE", None, &s.name, &perms);
            db.services.push(s);
        }
        for (r, perms) in self.routes {
            grant("ROUTE", None, &r.name, &perms);
            db.routes.push(r);
        }
        db.event_notifications = self.event_notifications;
        db.partition_functions = self.partition_functions;
        db.partition_schemes = self.partition_schemes;
        for (mut t, text_id) in self.ddl_triggers {
            if t.clr_method.is_none() {
                let (text, encrypted) = texts(text_id);
                t.encrypted |= encrypted;
                if !t.encrypted {
                    t.definition = text;
                }
            }
            db.ddl_triggers.push(t);
        }
        for (b, perms) in self.bindings {
            grant("REMOTE SERVICE BINDING", None, &b.name, &perms);
            db.service_bindings.push(b);
        }
        for (c, perms) in self.certificates {
            grant("CERTIFICATE", None, &c.name, &perms);
            db.certificates.push(c);
        }
        for (k, perms) in self.symmetric_keys {
            grant("SYMMETRIC KEY", None, &k.name, &perms);
            db.symmetric_keys.push(k);
        }
        for (k, perms) in self.asymmetric_keys {
            grant("ASYMMETRIC KEY", None, &k.name, &perms);
            db.asymmetric_keys.push(k);
        }
        for (f, perms) in self.fulltext_catalogs {
            grant("FULLTEXT CATALOG", None, &f.name, &perms);
            db.fulltext_catalogs.push(f);
        }
        db.fulltext_stoplists = self.stoplists;
        db.search_property_lists = self.property_lists;
        for (p, perms) in self.policies {
            grant("OBJECT", Some(&p.schema), &p.name, &perms);
            db.security_policies.push(p);
        }
        db.external_file_formats = self.file_formats;
        db.external_data_sources = self.data_sources;
        for (t, perms) in self.external_tables {
            grant("OBJECT", Some(&t.schema), &t.name, &perms);
            db.external_tables.push(t);
        }
        db.credentials = self.credentials;
        let by = |a: &str, b: &str| sql_order(a, b);
        db.assemblies.sort_by(|a, b| by(&a.name, &b.name));
        db.xml_schema_collections.sort_by(|a, b| by(&a.schema, &b.schema).then_with(|| by(&a.name, &b.name)));
        db.defaults.sort_by(|a, b| by(&a.schema, &b.schema).then_with(|| by(&a.name, &b.name)));
        db.rules.sort_by(|a, b| by(&a.schema, &b.schema).then_with(|| by(&a.name, &b.name)));
        db.message_types.sort_by(|a, b| by(&a.name, &b.name));
        db.contracts.sort_by(|a, b| by(&a.name, &b.name));
        db.queues.sort_by(|a, b| by(&a.schema, &b.schema).then_with(|| by(&a.name, &b.name)));
        db.services.sort_by(|a, b| by(&a.name, &b.name));
        db.routes.sort_by(|a, b| by(&a.name, &b.name));
        db.event_notifications.sort_by(|a, b| by(&a.name, &b.name));
        db.ddl_triggers.sort_by(|a, b| by(&a.name, &b.name));
        db.security_policies.sort_by(|a, b| by(&a.schema, &b.schema).then_with(|| by(&a.name, &b.name)));
    }
}

/// An object, column or class permission with its securable; one per column for column lists.
pub(super) fn grants(p: &RawPerm, class: &str, schema: Option<&str>, name: &str) -> Vec<Permission> {
    let Some(base) = permission(p) else { return Vec::new() };
    let base = Permission { class: class.into(), schema: schema.map(str::to_string), object: Some(name.into()), ..base };
    if p.columns.is_empty() {
        vec![base]
    } else {
        p.columns.iter().map(|c| Permission { column: Some(c.clone()), ..base.clone() }).collect()
    }
}
