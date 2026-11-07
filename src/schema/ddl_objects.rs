//! T-SQL for the objects of `catalog::objects` and the table parts they bring, in SQL Compare's
//! layout (CRLF between the clauses of one statement).

use super::ddl::{qname, sql_string, type_sql};
use crate::catalog::*;
use crate::db::{literal, quote};

fn owner_or_dbo(o: &Option<String>) -> String {
    quote(o.as_deref().unwrap_or("dbo"))
}

/// `0x…` with a line continuation after every 128 bytes.
pub fn hex_literal(hex: &str) -> String {
    let hex = hex.to_ascii_lowercase();
    let mut out = String::with_capacity(hex.len() + hex.len() / 128 + 4);
    out.push_str("0x");
    let chunks: Vec<&str> = hex.as_bytes().chunks(256).map(|c| std::str::from_utf8(c).unwrap_or_default()).collect();
    for (i, c) in chunks.iter().enumerate() {
        out.push_str(c);
        if i + 1 < chunks.len() {
            out.push_str("\\\r\n");
        }
    }
    // SQL Compare ends an image of a whole number of 128-byte lines with a line break.
    if !hex.is_empty() && hex.len().is_multiple_of(256) {
        out.push_str("\r\n");
    }
    out
}

// ---- CLR ----

/// CREATE ASSEMBLY, then VISIBILITY and the extra files.
pub fn create_assembly(a: &Assembly) -> Vec<String> {
    let q = quote(&a.name);
    let Some(main) = a.files.first() else { return vec!["-- assembly had no files".into()] };
    let mut out = vec![format!(
        "--Assembly {}\r\nCREATE ASSEMBLY {q}\r\nAUTHORIZATION {}\r\nFROM {}\r\nWITH PERMISSION_SET={}",
        a.clr_name,
        owner_or_dbo(&a.owner),
        hex_literal(&main.content),
        a.permission_set
    )];
    if !a.visible {
        out.push(format!("ALTER ASSEMBLY {q} WITH VISIBILITY = OFF"));
    }
    out.extend(a.files.iter().skip(1).map(|f| add_assembly_file(&a.name, f)));
    out
}

pub fn add_assembly_file(assembly: &str, f: &AssemblyFile) -> String {
    format!("ALTER ASSEMBLY {}\r\nADD FILE FROM\r\n{}\r\nAS '{}'", quote(assembly), hex_literal(&f.content), f.name)
}

/// ALTER ASSEMBLY for a new binary and/or new options; None when neither changes.
pub fn alter_assembly(a: &Assembly, t: &Assembly) -> Vec<String> {
    let q = quote(&a.name);
    let mut out = Vec::new();
    let binary_changed = a.files.first().map(|f| &f.content) != t.files.first().map(|f| &f.content);
    // Extra files go first when the main image changes, else the ones that change.
    for f in t.files.iter().skip(1) {
        if binary_changed || !a.files.iter().skip(1).any(|g| g.name.eq_ignore_ascii_case(&f.name) && g.content == f.content) {
            out.push(format!("ALTER ASSEMBLY {q}\r\nDROP FILE {}", literal(&f.name)));
        }
    }
    let mut with = Vec::new();
    if a.permission_set != t.permission_set {
        with.push(format!("PERMISSION_SET = {}", a.permission_set));
    }
    if a.visible != t.visible {
        with.push(format!("VISIBILITY = {}", if a.visible { "ON" } else { "OFF" }));
    }
    let with = if with.is_empty() { String::new() } else { format!("\r\nWITH {}", with.join(",\r\n")) };
    match a.files.first() {
        Some(main) if binary_changed => out.push(format!("ALTER ASSEMBLY {q}\r\nFROM {}{with}", hex_literal(&main.content))),
        _ if !with.is_empty() => out.push(format!("ALTER ASSEMBLY {q}{with}")),
        _ => {}
    }
    for f in a.files.iter().skip(1) {
        if binary_changed || !t.files.iter().skip(1).any(|g| g.name.eq_ignore_ascii_case(&f.name) && g.content == f.content) {
            out.push(add_assembly_file(&a.name, f));
        }
    }
    out
}

fn execute_as(e: &Option<String>) -> String {
    match e.as_deref() {
        None => "CALLER".into(),
        Some("OWNER") => "OWNER".into(),
        Some("SELF") => "SELF".into(),
        Some(u) => literal(u),
    }
}

fn parameters(r: &Routine) -> String {
    r.parameters
        .iter()
        .map(|p| format!("{} {}{}{}", p.name, p.data_type, p.default.as_ref().map(|d| format!("={d}")).unwrap_or_default(), if p.output { " OUTPUT" } else { "" }))
        .collect::<Vec<_>>()
        .join(", ")
}

/// CREATE (or ALTER, `verb`) of a CLR procedure, function or aggregate.
pub fn clr_routine(r: &Routine, verb: &str) -> String {
    let name = qname(&r.schema, &r.name);
    let method = r.clr_method.clone().unwrap_or_default();
    match r.kind {
        RoutineKind::ClrAggregate => format!(
            "{verb} AGGREGATE {name} ({})\r\nRETURNS {}\r\nEXTERNAL NAME {method}",
            parameters(r),
            r.returns.clone().unwrap_or_default()
        ),
        RoutineKind::ClrProcedure => {
            let params = if r.parameters.is_empty() { String::new() } else { format!(" ({})", parameters(r)) };
            format!("{verb} PROCEDURE {name}{params}\r\nWITH EXECUTE AS {}\r\nAS EXTERNAL NAME {method}", execute_as(&r.execute_as))
        }
        _ => {
            let returns = if r.kind == RoutineKind::ClrTableFunction {
                let cols: Vec<String> = r.result_columns.iter().map(|c| format!("{} {}", quote(&c.name), type_sql(&c.data_type))).collect();
                format!("TABLE (\r\n{})", cols.join(",\r\n"))
            } else {
                r.returns.clone().unwrap_or_default()
            };
            let null_input = if r.null_on_null_input { "\r\nRETURNS NULL ON NULL INPUT" } else { "" };
            format!(
                "{verb} FUNCTION {name} ({})\r\nRETURNS {returns}\r\nWITH EXECUTE AS {}{null_input}\r\nEXTERNAL NAME {method}",
                parameters(r),
                execute_as(&r.execute_as)
            )
        }
    }
}

/// CREATE TRIGGER for a CLR DML trigger.
pub fn clr_trigger(name: &ObjectName, table: &ObjectName, t: &Trigger, verb: &str) -> String {
    let when = if t.instead_of { "INSTEAD OF" } else { "FOR" };
    let nfr = if t.not_for_replication { " NOT FOR REPLICATION" } else { "" };
    format!(
        "{verb} TRIGGER {name} ON {table}\r\nWITH EXECUTE AS {}\r\n{when} {}{nfr}\r\nAS EXTERNAL NAME {}",
        execute_as(&t.execute_as),
        t.events.join(", "),
        t.clr_method.clone().unwrap_or_default()
    )
}

/// CREATE TRIGGER … ON DATABASE for a DDL trigger, T-SQL or CLR.
pub fn ddl_trigger(t: &DdlTrigger, verb: &str) -> String {
    let name = quote(&t.name);
    match &t.clr_method {
        None => super::ddl::module_sql(&t.definition, verb, &name),
        Some(method) => {
            let events: Vec<String> = t.events.chunks(3).map(|c| c.join(", ")).collect();
            format!(
                "{verb} TRIGGER {name}\r\nON DATABASE\r\nWITH EXECUTE AS {}\r\nAFTER {}\r\nAS EXTERNAL NAME {method}",
                execute_as(&t.execute_as),
                events.join(",\r\n")
            )
        }
    }
}

// ---- XML schema collections, defaults and rules ----

pub fn create_xml_schema_collection(x: &XmlSchemaCollection) -> String {
    format!("CREATE XML SCHEMA COLLECTION {}\r\nAS {}", qname(&x.schema, &x.name), sql_string(&x.definition))
}

/// CREATE DEFAULT / CREATE RULE from the stored text, naming the object as it is.
pub fn create_bindable(b: &BindableObject) -> String {
    super::ddl::module_sql(&b.definition, "CREATE", &qname(&b.schema, &b.name))
}

// ---- Partitioning ----

pub fn create_partition_function(f: &PartitionFunction) -> String {
    format!(
        "CREATE PARTITION FUNCTION {} ({})\r\nAS RANGE {}\r\nFOR VALUES ({})",
        quote(&f.name),
        type_sql(&f.data_type),
        if f.range_right { "RIGHT" } else { "LEFT" },
        f.boundaries.join(", ")
    )
}

pub fn create_partition_scheme(s: &PartitionScheme) -> String {
    format!(
        "CREATE PARTITION SCHEME {}\r\nAS PARTITION {}\r\nTO ({})",
        quote(&s.name),
        quote(&s.function),
        s.filegroups.iter().map(|f| quote(f)).collect::<Vec<_>>().join(", ")
    )
}

pub fn merge_range(function: &str, value: &str) -> String {
    format!("ALTER PARTITION FUNCTION {}()\r\nMERGE RANGE ({value})", quote(function))
}

pub fn split_range(function: &str, value: &str) -> String {
    format!("ALTER PARTITION FUNCTION {}()\r\nSPLIT RANGE ({value})", quote(function))
}

pub fn next_used(scheme: &str, filegroup: &str) -> String {
    format!("ALTER PARTITION SCHEME {} NEXT USED {}", quote(scheme), quote(filegroup))
}

// ---- Full-text search ----

pub fn create_fulltext_catalog(c: &FullTextCatalog) -> String {
    let mut s = format!("CREATE FULLTEXT CATALOG {}\r\nWITH ACCENT_SENSITIVITY = {}", quote(&c.name), if c.accent_sensitive { "ON" } else { "OFF" });
    if c.is_default {
        s.push_str("\r\nAS DEFAULT");
    }
    if let Some(o) = &c.owner {
        s.push_str(&format!("\r\nAUTHORIZATION {}", quote(o)));
    }
    s
}

pub fn stopword(list: &str, w: &Stopword, add: bool) -> String {
    format!("ALTER FULLTEXT STOPLIST {} {} {} LANGUAGE {};", quote(list), if add { "ADD" } else { "DROP" }, literal(&w.word), w.language)
}

pub fn create_stoplist(l: &FullTextStoplist) -> Vec<String> {
    let mut s = format!("CREATE FULLTEXT STOPLIST {}", quote(&l.name));
    if let Some(o) = &l.owner {
        s.push_str(&format!("\r\nAUTHORIZATION {}", quote(o)));
    }
    s.push(';');
    let mut out = vec![s];
    out.extend(l.words.iter().map(|w| stopword(&l.name, w, true)));
    out
}

pub fn add_search_property(list: &str, p: &SearchProperty) -> String {
    let description = p.description.as_ref().map(|d| format!(", PROPERTY_DESCRIPTION = {}", literal(d))).unwrap_or_default();
    format!(
        "ALTER SEARCH PROPERTY LIST {} ADD {}\r\n   WITH ( PROPERTY_SET_GUID = '{}', PROPERTY_INT_ID = {}{description} );",
        quote(list),
        literal(&p.name),
        p.set_guid,
        p.int_id
    )
}

pub fn create_property_list(l: &SearchPropertyList) -> Vec<String> {
    let mut out = vec![format!("CREATE SEARCH PROPERTY LIST {};", quote(&l.name))];
    out.extend(l.properties.iter().map(|p| add_search_property(&l.name, p)));
    out
}

pub fn fulltext_column(c: &FullTextColumn) -> String {
    let type_column = c.type_column.as_ref().map(|t| format!(" TYPE COLUMN {}", quote(t))).unwrap_or_default();
    format!("{}{type_column} LANGUAGE {}", quote(&c.name), c.language)
}

/// CREATE FULLTEXT INDEX, then its columns and ENABLE. `filegroups` writes FILEGROUP;
/// `no_population` is AddNoPopulation; `property_lists` whether the target has them; `inline`
/// (InlineFulltextFields) lists the columns in the CREATE instead of one ALTER … ADD each.
pub fn create_fulltext_index(table: &ObjectName, f: &FullTextIndex, filegroups: bool, no_population: bool, property_lists: bool, inline: bool) -> Vec<String> {
    let t = table.quoted();
    let on = match f.filegroup.as_ref().filter(|_| filegroups) {
        Some(fg) => format!("({}, FILEGROUP {})", quote(&f.catalog), quote(fg)),
        None => quote(&f.catalog),
    };
    let mut with = Vec::new();
    if no_population {
        with.push("CHANGE_TRACKING OFF, NO POPULATION".to_string());
    } else if f.change_tracking != "AUTO" {
        with.push(format!("CHANGE_TRACKING {}", f.change_tracking));
    }
    match f.stoplist.as_str() {
        "SYSTEM" => {}
        "OFF" => with.push("STOPLIST OFF".into()),
        name => with.push(format!("STOPLIST {}", quote(name))),
    }
    if let Some(pl) = f.property_list.as_ref().filter(|_| property_lists) {
        with.push(format!("SEARCH PROPERTY LIST {}", quote(pl)));
    }
    let with = if with.is_empty() { String::new() } else { format!(" WITH {}", with.join(", ")) };
    let columns = if inline && !f.columns.is_empty() {
        format!("\r\n(\r\n{}\r\n)", f.columns.iter().map(fulltext_column).collect::<Vec<_>>().join(",\r\n"))
    } else {
        String::new()
    };
    let mut out = vec![format!("CREATE FULLTEXT INDEX ON {t}{columns} KEY INDEX {} ON {on}{with}", quote(&f.key_index))];
    for c in f.columns.iter().filter(|_| !inline) {
        out.push(format!("ALTER FULLTEXT INDEX ON {t} ADD ({})", fulltext_column(c)));
    }
    if !f.columns.is_empty() && !no_population {
        out.push(format!("ALTER FULLTEXT INDEX ON {t} ENABLE"));
    }
    out
}

// ---- Statistics and edge constraints ----

pub fn create_statistics(table: &ObjectName, s: &Statistic, incremental: bool) -> String {
    let cols = s.columns.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", ");
    let mut sql = format!("CREATE STATISTICS {} ON {} ({cols})", quote(&s.name), table.quoted());
    if let Some(f) = &s.filter {
        sql.push_str(&format!(" WHERE {f}"));
    }
    let mut with = Vec::new();
    if s.no_recompute {
        with.push("NORECOMPUTE");
    }
    if s.incremental && incremental && s.filter.is_none() {
        with.push("INCREMENTAL=ON");
    }
    if !with.is_empty() {
        sql.push_str(&format!(" WITH {}", with.join(", ")));
    }
    sql
}

pub fn drop_statistics(table: &ObjectName, name: &str) -> String {
    format!("DROP STATISTICS {}.{}", table.quoted(), quote(name))
}

pub fn add_edge_constraint(table: &ObjectName, e: &EdgeConstraint, with_nocheck: bool) -> String {
    let conns = e.connections.iter().map(|(a, b)| format!("{a} TO {b}")).collect::<Vec<_>>().join(", ");
    let nocheck = if with_nocheck { " WITH NOCHECK" } else { "" };
    let cascade = if e.on_delete_cascade { " ON DELETE CASCADE" } else { "" };
    format!("ALTER TABLE {}{nocheck} ADD CONSTRAINT {} CONNECTION ({conns}){cascade}", table.quoted(), quote(&e.name))
}

// ---- Sensitivity classification ----

pub fn add_sensitivity(table: &ObjectName, column: &str, s: &Sensitivity) -> String {
    let mut items = Vec::new();
    // A label id needs a label (and an information type id a type): the id stands in.
    let label = s.label.clone().or_else(|| s.label_id.clone());
    let info = s.information_type.clone().or_else(|| s.information_type_id.clone());
    if let Some(v) = &label {
        items.push(format!("LABEL = {}", literal(v)));
    }
    if let Some(v) = &s.label_id {
        items.push(format!("LABEL_ID = {}", literal(v)));
    }
    if let Some(v) = &info {
        items.push(format!("INFORMATION_TYPE = {}", literal(v)));
    }
    if let Some(v) = &s.information_type_id {
        items.push(format!("INFORMATION_TYPE_ID = {}", literal(v)));
    }
    if let Some(v) = &s.rank {
        items.push(format!("RANK = {v}"));
    }
    format!("ADD SENSITIVITY CLASSIFICATION TO {}.{} WITH ({})", table.quoted(), quote(column), items.join(", "))
}

pub fn drop_sensitivity(table: &ObjectName, column: &str) -> String {
    format!("DROP SENSITIVITY CLASSIFICATION FROM {}.{}", table.quoted(), quote(column))
}

// ---- Service Broker ----

fn validation(m: &MessageType, one_line: bool) -> String {
    match (m.validation.as_str(), &m.xml_collection) {
        ("VALID_XML", Some(c)) => format!("VALID_XML{}WITH SCHEMA COLLECTION {c}", if one_line { " " } else { "\r\n" }),
        (v, _) => v.to_string(),
    }
}

pub fn create_message_type(m: &MessageType) -> String {
    format!("CREATE MESSAGE TYPE {}\r\nAUTHORIZATION {}\r\nVALIDATION={}", quote(&m.name), owner_or_dbo(&m.owner), validation(m, false))
}

pub fn alter_message_type(m: &MessageType) -> String {
    format!("ALTER MESSAGE TYPE {}\r\nVALIDATION={}", quote(&m.name), validation(m, true))
}

pub fn create_contract(c: &Contract) -> String {
    let usages = c.messages.iter().map(|m| format!("{} SENT BY {}", quote(&m.message_type), m.sent_by)).collect::<Vec<_>>().join(",\r\n");
    format!("CREATE CONTRACT {}\r\nAUTHORIZATION {} (\r\n{usages}\r\n)", quote(&c.name), owner_or_dbo(&c.owner))
}

fn on_off(b: bool) -> &'static str {
    if b { "ON" } else { "OFF" }
}

fn queue_execute_as(e: &Option<String>) -> String {
    match e.as_deref() {
        None => "CALLER".into(),
        Some("OWNER") => "OWNER".into(),
        Some("SELF") => "SELF".into(),
        Some(u) => literal(u),
    }
}

/// CREATE QUEUE; `poison` whether the target takes POISON_MESSAGE_HANDLING, `filegroup` whether
/// to write ON [fg].
pub fn create_queue(q: &Queue, poison: bool, filegroup: bool) -> String {
    let mut s = format!("CREATE QUEUE {}\r\nWITH STATUS={},\r\nRETENTION={}", qname(&q.schema, &q.name), on_off(q.enabled), on_off(q.retention));
    if poison {
        s.push_str(&format!(",\r\nPOISON_MESSAGE_HANDLING (STATUS={})", on_off(q.poison_message_handling)));
    }
    if let Some(a) = &q.activation {
        s.push_str(&format!(
            ",\r\nACTIVATION (\r\nSTATUS={},\r\nPROCEDURE_NAME={},\r\nMAX_QUEUE_READERS={},\r\nEXECUTE AS {}\r\n)",
            on_off(a.enabled),
            a.procedure,
            a.max_readers,
            queue_execute_as(&a.execute_as)
        ));
    }
    if let Some(fg) = q.filegroup.as_ref().filter(|_| filegroup) {
        s.push_str(&format!("\r\nON {}", quote(fg)));
    }
    s
}

/// ALTER QUEUE with the clauses that change; None when nothing does.
pub fn alter_queue(q: &Queue, t: &Queue, poison: bool) -> Option<String> {
    let mut items = Vec::new();
    if q.enabled != t.enabled {
        items.push(format!("STATUS = {}", on_off(q.enabled)));
    }
    if q.retention != t.retention {
        items.push(format!("RETENTION = {}", on_off(q.retention)));
    }
    if poison && q.poison_message_handling != t.poison_message_handling {
        items.push(format!("POISON_MESSAGE_HANDLING (STATUS={})", on_off(q.poison_message_handling)));
    }
    match (&q.activation, &t.activation) {
        (None, Some(_)) => items.push("ACTIVATION (DROP)".into()),
        (Some(a), b) => {
            let b = b.as_ref();
            let mut act = Vec::new();
            if b.is_none_or(|b| b.enabled != a.enabled) {
                act.push(format!("STATUS={}", on_off(a.enabled)));
            }
            if b.is_none_or(|b| !b.procedure.eq_ignore_ascii_case(&a.procedure)) {
                act.push(format!("PROCEDURE_NAME={}", a.procedure));
            }
            if b.is_none_or(|b| b.max_readers != a.max_readers) {
                act.push(format!("MAX_QUEUE_READERS={}", a.max_readers));
            }
            if b.is_none_or(|b| b.execute_as != a.execute_as) {
                act.push(format!("EXECUTE AS {}", queue_execute_as(&a.execute_as)));
            }
            if !act.is_empty() {
                items.push(format!("ACTIVATION ({})", act.join(", ")));
            }
        }
        (None, None) => {}
    }
    (!items.is_empty()).then(|| format!("ALTER QUEUE {} WITH {}", qname(&q.schema, &q.name), items.join(", ")))
}

pub fn create_service(s: &Service) -> String {
    let mut sql = format!("CREATE SERVICE {}\r\nAUTHORIZATION {}\r\nON QUEUE {}", quote(&s.name), owner_or_dbo(&s.owner), s.queue.quoted());
    if !s.contracts.is_empty() {
        sql.push_str(&format!("\r\n(\r\n{}\r\n)", s.contracts.iter().map(|c| quote(c)).collect::<Vec<_>>().join(",\r\n")));
    }
    sql
}

/// ALTER SERVICE for a new queue and added or dropped contracts.
pub fn alter_service(s: &Service, t: &Service) -> Vec<String> {
    let mut out = Vec::new();
    let q = quote(&s.name);
    if !s.queue.quoted().eq_ignore_ascii_case(&t.queue.quoted()) {
        out.push(format!("ALTER SERVICE {q} ON QUEUE {}", s.queue.quoted()));
    }
    for c in s.contracts.iter().filter(|c| !t.contracts.iter().any(|x| x.eq_ignore_ascii_case(c))) {
        out.push(format!("ALTER SERVICE {q}\r\nON QUEUE {}\r\n(\r\n ADD CONTRACT {}\r\n)", s.queue.quoted(), quote(c)));
    }
    for c in t.contracts.iter().filter(|c| !s.contracts.iter().any(|x| x.eq_ignore_ascii_case(c))) {
        out.push(format!("ALTER SERVICE {q}\r\nON QUEUE {}\r\n(\r\n DROP CONTRACT {}\r\n)", s.queue.quoted(), quote(c)));
    }
    out
}

fn route_items(r: &Route) -> Vec<String> {
    let mut items = Vec::new();
    if let Some(s) = &r.remote_service {
        items.push(format!("SERVICE_NAME={}", literal(s)));
    }
    if let Some(b) = &r.broker_instance {
        items.push(format!("BROKER_INSTANCE={}", literal(b)));
    }
    items
}

/// CREATE ROUTE; a route with a lifetime gets the seconds left until its expiry, worked out
/// when the script runs.
pub fn create_route(r: &Route) -> String {
    let mut items = vec![format!("ADDRESS={}", literal(&r.address))];
    items.extend(route_items(r));
    if let Some(m) = &r.mirror_address {
        items.push(format!("MIRROR_ADDRESS={}", literal(m)));
    }
    let create = format!("CREATE ROUTE {}\r\nAUTHORIZATION {}\r\nWITH {}", quote(&r.name), owner_or_dbo(&r.owner), items.join(",\r\n"));
    match &r.expires {
        None => create,
        Some(at) => format!(
            "DECLARE @RouteLifetime INT\r\nDECLARE @CreateRouteCommand Nvarchar(4000)\r\nSET @RouteLifetime = CASE WHEN (DATEDIFF(s, GETUTCDATE(), CAST(N'{at}' AS DATETIME)) < 0)\r\nTHEN 1 ELSE DATEDIFF(s, GETUTCDATE(), CAST(N'{at}' AS DATETIME)) END\r\nset @CreateRouteCommand = {}+ cast(@RouteLifetime as nvarchar(12))\r\nexec sp_executesql @CreateRouteCommand",
            sql_string(&format!("{create},\r\nLIFETIME="))
        ),
    }
}

/// ALTER ROUTE for what can change in place.
pub fn alter_route(r: &Route, t: &Route) -> Option<String> {
    let mut items = Vec::new();
    if r.remote_service != t.remote_service
        && let Some(s) = &r.remote_service
    {
        items.push(format!("SERVICE_NAME={}", literal(s)));
    }
    if r.broker_instance != t.broker_instance
        && let Some(b) = &r.broker_instance
    {
        items.push(format!("BROKER_INSTANCE={}", literal(b)));
    }
    if r.address != t.address {
        items.push(format!("ADDRESS={}", literal(&r.address)));
    }
    if r.mirror_address != t.mirror_address
        && let Some(m) = &r.mirror_address
    {
        items.push(format!("MIRROR_ADDRESS={}", literal(m)));
    }
    (!items.is_empty()).then(|| format!("ALTER ROUTE {} WITH {}", quote(&r.name), items.join(",\r\n")))
}

pub fn create_service_binding(b: &ServiceBinding) -> String {
    format!(
        "CREATE REMOTE SERVICE BINDING {}\r\nAUTHORIZATION {}\r\nTO SERVICE '{}'\r\nWITH USER ={},\r\nANONYMOUS = {}",
        quote(&b.name),
        owner_or_dbo(&b.owner),
        b.remote_service,
        quote(&b.user),
        on_off(b.anonymous)
    )
}

pub fn alter_service_binding(b: &ServiceBinding, t: &ServiceBinding) -> Option<String> {
    let mut items = Vec::new();
    if !b.user.eq_ignore_ascii_case(&t.user) {
        items.push(format!("USER = {}", quote(&b.user)));
    }
    if b.anonymous != t.anonymous {
        items.push(format!("ANONYMOUS = {}", on_off(b.anonymous)));
    }
    (!items.is_empty()).then(|| format!("ALTER REMOTE SERVICE BINDING {}\r\nWITH {}", quote(&b.name), items.join(",\r\n")))
}

fn notification_target(e: &EventNotification) -> String {
    match &e.queue {
        Some(q) => format!("QUEUE {q}"),
        None => "DATABASE".into(),
    }
}

pub fn create_event_notification(e: &EventNotification) -> String {
    let instance = if e.broker_instance.is_empty() { String::new() } else { format!(", '{}'", e.broker_instance) };
    format!(
        "CREATE EVENT NOTIFICATION {}\r\nON {}{}\r\nFOR {}\r\nTO SERVICE '{}'{instance}",
        quote(&e.name),
        notification_target(e),
        if e.fan_in { " WITH FAN_IN" } else { "" },
        e.events.join(", "),
        e.service
    )
}

pub fn drop_event_notification(e: &EventNotification) -> String {
    format!("DROP EVENT NOTIFICATION {} ON {}", quote(&e.name), notification_target(e))
}

// ---- Keys and certificates (described, never scripted: their secrets can't be read) ----

fn private_key_text(code: &str) -> &'static str {
    match code {
        "SK" => "Encrypted by service master key",
        "MK" => "Encrypted by master key",
        "PW" => "Encrypted by password",
        "NA" => "No private key",
        _ => "Encrypted by Other Encryption (e.g. Hardware Crypto Device)",
    }
}

pub fn describe_certificate(c: &Certificate) -> String {
    format!(
        "-- Certificate {}\r\n--   Authorization: {}\r\n--   Subject: {}\r\n--   Thumbprint: {}\r\n--   Start date: {}Z\r\n--   Expiry date: {}Z\r\n--   Issuer: {}\r\n--   Private key encryption: {}\r\n--   Active for dialog: {}\r\n-- This certificate cannot be synchronized.",
        quote(&c.name),
        c.owner.as_deref().unwrap_or("dbo"),
        c.subject,
        c.thumbprint,
        c.start_date,
        c.expiry_date,
        c.issuer,
        private_key_text(&c.private_key),
        on_off(c.active_for_dialog)
    )
}

pub fn describe_asymmetric_key(k: &AsymmetricKey) -> String {
    let algorithm = match k.algorithm.as_str() {
        "RSA_512" => "RSA 512",
        "RSA_1024" => "RSA 1024",
        "RSA_2048" => "RSA 2048",
        "RSA_3072" => "RSA 3072",
        "RSA_4096" => "RSA 4096",
        _ => "Unknown Algorithm - possibly hardware?",
    };
    format!(
        "-- Asymmetric key {}\r\n--   Authorization: {}\r\n--   Thumbprint: {}\r\n--   Private key encryption type: {}\r\n--   Key encryption algorithm: {algorithm}\r\n--   Key length: {}\r\n-- This asymmetric key cannot be synchronized.",
        quote(&k.name),
        k.owner.as_deref().unwrap_or("dbo"),
        k.thumbprint,
        private_key_text(&k.private_key),
        k.key_length
    )
}

pub fn describe_symmetric_key(k: &SymmetricKey) -> String {
    let algorithm = match k.algorithm.as_str() {
        "RC2" => "RC 2",
        "RC4" | "RC4_128" => "RC 4",
        "DES" => "DES",
        "TRIPLE_DES" | "TRIPLE_DES_3KEY" => "Triple DES",
        "DESX" => "DESX",
        "AES_128" => "AES 128",
        "AES_192" => "AES 192",
        "AES_256" => "AES 256",
        other => other,
    };
    format!(
        "-- Symmetric key {}\r\n--   Authorization: {}\r\n--   Key algorithm: {algorithm}\r\n--   Key length: {}\r\n--   Key GUID: {}\r\n--   Creation date: {}\r\n--   Modification date: {}\r\n-- This symmetric key cannot be synchronized.",
        quote(&k.name),
        k.owner.as_deref().unwrap_or("dbo"),
        k.key_length,
        k.key_guid,
        k.create_date,
        k.modify_date
    )
}

// ---- Row-level security ----

fn predicate(p: &SecurityPredicate, verb: &str) -> String {
    let op = p.operation.as_ref().map(|o| format!(" {o}")).unwrap_or_default();
    format!("{verb} {} PREDICATE {}{}\r\nON {}{op}", p.kind, p.function, p.arguments, p.table)
}

pub fn create_security_policy(p: &SecurityPolicy) -> String {
    let predicates = p.predicates.iter().map(|x| predicate(x, "ADD")).collect::<Vec<_>>().join(",\r\n");
    let binding = if p.schema_bound { "" } else { ", SCHEMABINDING = OFF" };
    let nfr = if p.not_for_replication { "\r\nNOT FOR REPLICATION" } else { "" };
    format!("CREATE SECURITY POLICY {}\r\n{predicates}\r\nWITH (STATE = {}{binding}){nfr}", qname(&p.schema, &p.name), on_off(p.enabled))
}

/// ALTER SECURITY POLICY: dropped, changed and added predicates, then STATE and NOT FOR REPLICATION.
pub fn alter_security_policy(p: &SecurityPolicy, t: &SecurityPolicy) -> Vec<String> {
    let name = qname(&p.schema, &p.name);
    let slot = |x: &SecurityPredicate, y: &SecurityPredicate| x.kind == y.kind && x.operation == y.operation && x.table.to_string().eq_ignore_ascii_case(&y.table.to_string());
    let mut out = Vec::new();
    for y in t.predicates.iter().filter(|y| !p.predicates.iter().any(|x| slot(x, y))) {
        let op = y.operation.as_ref().map(|o| format!(" {o}")).unwrap_or_default();
        out.push(format!("ALTER SECURITY POLICY {name}\r\nDROP {} PREDICATE\r\nON {}{op}", y.kind, y.table));
    }
    for x in &p.predicates {
        match t.predicates.iter().find(|y| slot(x, y)) {
            Some(y) if y.function != x.function || y.arguments != x.arguments => out.push(format!("ALTER SECURITY POLICY {name}\r\n{}", predicate(x, "ALTER"))),
            Some(_) => {}
            None => out.push(format!("ALTER SECURITY POLICY {name}\r\n{}", predicate(x, "ADD"))),
        }
    }
    if p.enabled != t.enabled {
        out.push(format!("ALTER SECURITY POLICY {name}\r\nWITH (STATE = {})", on_off(p.enabled)));
    }
    if p.not_for_replication != t.not_for_replication {
        out.push(format!("ALTER SECURITY POLICY {name}\r\n{} NOT FOR REPLICATION", if p.not_for_replication { "ADD" } else { "DROP" }));
    }
    out
}

// ---- PolyBase ----

pub fn create_external_data_source(x: &ExternalDataSource) -> String {
    let mut items = vec![format!("LOCATION = {}", sql_string(&x.location))];
    if let Some(o) = &x.connection_options {
        items.push(format!("CONNECTION_OPTIONS = {}", sql_string(o)));
    }
    if let Some(c) = &x.credential {
        items.push(format!("CREDENTIAL = {}", quote(c)));
    }
    if x.pushdown == Some(false) {
        items.push("PUSHDOWN = OFF".into());
    }
    if let Some(k) = &x.kind {
        items.push(format!("TYPE = {k}"));
    }
    if let Some(d) = &x.database_name {
        items.push(format!("DATABASE_NAME = {}", sql_string(d)));
    }
    if let Some(s) = &x.shard_map_name {
        items.push(format!("SHARD_MAP_NAME = {}", sql_string(s)));
    }
    if let Some(r) = &x.resource_manager_location {
        items.push(format!("RESOURCE_MANAGER_LOCATION = {}", sql_string(r)));
    }
    format!("CREATE EXTERNAL DATA SOURCE {} WITH\r\n(\r\n{}\r\n)", quote(&x.name), items.join(",\r\n"))
}

pub fn create_external_file_format(x: &ExternalFileFormat) -> String {
    let mut options = Vec::new();
    if let Some(v) = &x.field_terminator {
        options.push(format!("FIELD_TERMINATOR = {}", sql_string(v)));
    }
    if let Some(v) = &x.string_delimiter {
        options.push(format!("STRING_DELIMITER = {}", sql_string(v)));
    }
    if let Some(v) = x.first_row {
        options.push(format!("FIRST_ROW = {v}"));
    }
    if let Some(v) = &x.date_format {
        options.push(format!("DATE_FORMAT = {}", sql_string(v)));
    }
    if x.format_type == "DELIMITEDTEXT" {
        options.push(format!("USE_TYPE_DEFAULT = {}", if x.use_type_default { "TRUE" } else { "FALSE" }));
    }
    if let Some(v) = &x.encoding {
        options.push(format!("ENCODING = {}", sql_string(v)));
    }
    let mut items = vec![format!("FORMAT_TYPE =  {}", x.format_type)];
    if !options.is_empty() {
        items.push(format!("FORMAT_OPTIONS\r\n(\r\n{}\r\n)", options.join(",\r\n")));
    }
    if let Some(v) = &x.data_compression {
        items.push(format!("DATA_COMPRESSION = {}", sql_string(v)));
    }
    if let Some(v) = &x.serde_method {
        items.push(format!("SERDE_METHOD = {}", sql_string(v)));
    }
    format!("CREATE EXTERNAL FILE FORMAT {} WITH\r\n(\r\n{}\r\n)", quote(&x.name), items.join(",\r\n"))
}

pub fn create_external_table(x: &ExternalTable) -> String {
    let r = super::ddl::Render::display();
    let cols: Vec<String> = x.columns.iter().map(|c| super::ddl::column_definition_with(c, false, &r, false)).collect();
    let mut items = Vec::new();
    if let Some(v) = &x.location {
        items.push(format!("LOCATION = {}", sql_string(v)));
    }
    items.push(format!("DATA_SOURCE = {}", quote(&x.data_source)));
    if let Some(v) = &x.file_format {
        items.push(format!("FILE_FORMAT = {}", quote(v)));
    }
    if let Some(v) = &x.reject_type {
        items.push(format!("REJECT_TYPE = {}", v.to_lowercase()));
    }
    if let Some(v) = &x.reject_value {
        items.push(format!("REJECT_VALUE = {v}"));
    }
    if let Some(v) = x.reject_sample_value.as_ref().filter(|_| x.reject_type.as_deref() == Some("PERCENTAGE")) {
        items.push(format!("REJECT_SAMPLE_VALUE = {v}"));
    }
    if let Some(v) = &x.remote_schema {
        items.push(format!("SCHEMA_NAME = {}", sql_string(v)));
    }
    if let Some(v) = &x.remote_object {
        items.push(format!("OBJECT_NAME = {}", sql_string(v)));
    }
    if let Some(v) = &x.distribution {
        items.push(format!("DISTRIBUTION = {v}"));
    }
    format!("CREATE EXTERNAL TABLE {}\r\n(\r\n{}\r\n)\r\nWITH\r\n(\r\n{}\r\n)", qname(&x.schema, &x.name), cols.join(",\r\n"), items.join(",\r\n"))
}

// ---- Object definitions for display ----

/// The statements for a table's statistics, full-text index and edge constraints;
/// `inline_fulltext` is InlineFulltextFields.
pub fn table_parts(t: &Table, inline_fulltext: bool) -> Vec<String> {
    let name = t.object_name();
    let mut out: Vec<String> = t.statistics.iter().map(|s| create_statistics(&name, s, true)).collect();
    out.extend(t.edge_constraints.iter().map(|e| add_edge_constraint(&name, e, false)));
    if let Some(f) = &t.fulltext_index {
        out.extend(create_fulltext_index(&name, f, true, false, true, inline_fulltext));
    }
    for c in &t.columns {
        if let Some(s) = &c.sensitivity {
            out.push(add_sensitivity(&name, &c.name, s));
        }
    }
    out
}

/// The SQL of one of the objects of this module, for the side-by-side view.
pub fn object_sql(db: &Database, ty: super::compare::ObjectType, name: &ObjectName) -> Option<String> {
    use super::compare::ObjectType as T;
    let same = |s: &str, n: &str| s == name.schema && n == name.name;
    let named = |n: &str| n == name.name;
    let join = |v: Vec<String>| v.join("\nGO\n");
    match ty {
        T::Assembly => db.assemblies.iter().find(|a| named(&a.name)).map(|a| join(create_assembly(a))),
        T::XmlSchemaCollection => db.xml_schema_collections.iter().find(|x| same(&x.schema, &x.name)).map(create_xml_schema_collection),
        T::Default => db.defaults.iter().find(|x| same(&x.schema, &x.name)).map(create_bindable),
        T::Rule => db.rules.iter().find(|x| same(&x.schema, &x.name)).map(create_bindable),
        T::DdlTrigger => db.ddl_triggers.iter().find(|t| named(&t.name)).map(|t| {
            if t.encrypted { "-- The object is encrypted.".into() } else { ddl_trigger(t, "CREATE") }
        }),
        T::PartitionFunction => db.partition_functions.iter().find(|f| named(&f.name)).map(create_partition_function),
        T::PartitionScheme => db.partition_schemes.iter().find(|f| named(&f.name)).map(create_partition_scheme),
        T::FullTextCatalog => db.fulltext_catalogs.iter().find(|f| named(&f.name)).map(create_fulltext_catalog),
        T::FullTextStoplist => db.fulltext_stoplists.iter().find(|f| named(&f.name)).map(|l| join(create_stoplist(l))),
        T::SearchPropertyList => db.search_property_lists.iter().find(|f| named(&f.name)).map(|l| join(create_property_list(l))),
        T::MessageType => db.message_types.iter().find(|f| named(&f.name)).map(create_message_type),
        T::Contract => db.contracts.iter().find(|f| named(&f.name)).map(create_contract),
        T::Queue => db.queues.iter().find(|q| same(&q.schema, &q.name)).map(|q| {
            let mut v = vec![create_queue(q, true, true)];
            v.extend(q.event_notifications.iter().map(create_event_notification));
            join(v)
        }),
        T::Service => db.services.iter().find(|f| named(&f.name)).map(create_service),
        T::Route => db.routes.iter().find(|f| named(&f.name)).map(create_route),
        T::ServiceBinding => db.service_bindings.iter().find(|f| named(&f.name)).map(create_service_binding),
        T::EventNotification => db.event_notifications.iter().find(|f| named(&f.name)).map(create_event_notification),
        T::Certificate => db.certificates.iter().find(|f| named(&f.name)).map(describe_certificate),
        T::AsymmetricKey => db.asymmetric_keys.iter().find(|f| named(&f.name)).map(describe_asymmetric_key),
        T::SymmetricKey => db.symmetric_keys.iter().find(|f| named(&f.name)).map(describe_symmetric_key),
        T::SecurityPolicy => db.security_policies.iter().find(|p| same(&p.schema, &p.name)).map(create_security_policy),
        T::ExternalDataSource => db.external_data_sources.iter().find(|f| named(&f.name)).map(create_external_data_source),
        T::ExternalFileFormat => db.external_file_formats.iter().find(|f| named(&f.name)).map(create_external_file_format),
        T::ExternalTable => db.external_tables.iter().find(|t| same(&t.schema, &t.name)).map(create_external_table),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_wraps_every_128_bytes() {
        let hex = "AB".repeat(130);
        let s = hex_literal(&hex);
        assert!(s.starts_with("0xabab") && s.contains("\\\r\n"));
        assert_eq!(s.matches("\\\r\n").count(), 1);
        assert!(!hex_literal(&"AB".repeat(128)).contains('\\'));
    }

    #[test]
    fn broker_objects() {
        let m = MessageType { name: "//Shop/Order".into(), validation: "WELL_FORMED_XML".into(), ..Default::default() };
        assert_eq!(create_message_type(&m), "CREATE MESSAGE TYPE [//Shop/Order]\r\nAUTHORIZATION [dbo]\r\nVALIDATION=WELL_FORMED_XML");
        let c = Contract { name: "//Shop/C".into(), messages: vec![ContractMessage { message_type: "//Shop/Order".into(), sent_by: "INITIATOR".into() }], ..Default::default() };
        assert_eq!(create_contract(&c), "CREATE CONTRACT [//Shop/C]\r\nAUTHORIZATION [dbo] (\r\n[//Shop/Order] SENT BY INITIATOR\r\n)");
        let q = Queue { schema: "dbo".into(), name: "Q".into(), ..Default::default() };
        assert_eq!(create_queue(&q, true, false), "CREATE QUEUE [dbo].[Q]\r\nWITH STATUS=ON,\r\nRETENTION=OFF,\r\nPOISON_MESSAGE_HANDLING (STATUS=ON)");
        let mut q2 = q.clone();
        q2.enabled = false;
        assert_eq!(alter_queue(&q2, &q, true).unwrap(), "ALTER QUEUE [dbo].[Q] WITH STATUS = OFF");
    }

    #[test]
    fn security_policies() {
        let pr = SecurityPredicate { kind: "FILTER".into(), function: "[rls].[f]".into(), arguments: "([TenantId])".into(), table: ObjectName::new("dbo", "T"), operation: None };
        let p = SecurityPolicy { schema: "rls".into(), name: "P".into(), enabled: true, schema_bound: true, predicates: vec![pr], ..Default::default() };
        assert_eq!(create_security_policy(&p), "CREATE SECURITY POLICY [rls].[P]\r\nADD FILTER PREDICATE [rls].[f]([TenantId])\r\nON [dbo].[T]\r\nWITH (STATE = ON)");
    }
}
