//! Fill-in-the-blanks: find the variable slots in a command, and work out what
//! each one should default to.
//!
//! Two halves that share one rules table (`FLAG_RULES`):
//!   * `detect()`   — finds slots in a corpus command (three tiers: explicit
//!                    `<TOKEN>`, the canonical ALL_CAPS allowlist, contextual
//!                    flags, then literal lab values).
//!   * `VarContext` — harvests concrete values off the machine (sticky store,
//!                    /etc/hosts, shell history, env, local tunnel IP) using the
//!                    same flag rules in reverse.
//!
//! The allowlist matters: the corpus is full of SQL keywords (`SELECT`, `FROM`),
//! HTTP verbs (`POST`, `GET`) and registry hives (`HKLM`) that a generic
//! `[A-Z_]+` regex would happily mistake for placeholders.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

// ---------------------------------------------------------------- kinds

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    Ip,
    Fqdn,
    Host,
    Domain,
    User,
    Pass,
    Hash,
    Port,
    Iface,
    File,
    LocalIp,
    Other,
}

impl VarKind {
    /// Kinds derived from the active /etc/hosts target.
    pub fn from_target(self) -> bool {
        matches!(self, VarKind::Ip | VarKind::Fqdn | VarKind::Host | VarKind::Domain)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Sticky,
    Hosts,
    History,
    Env,
    LocalIp,
    Literal,
    Empty,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Origin::Sticky => "last used",
            Origin::Hosts => "hosts",
            Origin::History => "recent",
            Origin::Env => "env",
            Origin::LocalIp => "local",
            Origin::Literal => "literal",
            Origin::Empty => "",
        }
    }
}

// ---------------------------------------------------------------- tables

/// Canonical placeholder tokens, seeded from the measured corpus frequencies.
/// `(token, kind, canon)` — `canon` is the sticky-store key, so `USER`,
/// `USERNAME` and `-u` all share one remembered value.
const CANON: &[(&str, VarKind, &str)] = &[
    // identity
    ("USERNAME", VarKind::User, "user"),
    ("TARGET_USER", VarKind::User, "target_user"),
    ("ADMIN_USER", VarKind::User, "admin_user"),
    ("USER1", VarKind::User, "user1"),
    ("USER2", VarKind::User, "user2"),
    ("USER3", VarKind::User, "user3"),
    ("USER", VarKind::User, "user"),
    ("UPN", VarKind::User, "upn"),
    ("PRINCIPAL", VarKind::User, "principal"),
    ("COMPUTER", VarKind::Host, "computer"),
    // secrets
    ("PASSWORD", VarKind::Pass, "pass"),
    ("PASS", VarKind::Pass, "pass"),
    ("NTLM_HASH", VarKind::Hash, "hash"),
    ("AES_KEY", VarKind::Hash, "aes_key"),
    ("DOMAIN_SID", VarKind::Other, "domain_sid"),
    ("TICKET_B64", VarKind::Other, "ticket_b64"),
    // domain / DC
    ("DOMAIN", VarKind::Domain, "domain"),
    ("DC_FQDN", VarKind::Fqdn, "dc_fqdn"),
    ("DC_HOSTNAME", VarKind::Host, "dc_host"),
    ("DC_HOST", VarKind::Host, "dc_host"),
    ("DC_IP", VarKind::Ip, "dc_ip"),
    ("CA_HOST", VarKind::Host, "ca_host"),
    ("CA_NAME", VarKind::Other, "ca_name"),
    ("GPO_NAME", VarKind::Other, "gpo_name"),
    ("TARGET_GROUP_DN", VarKind::Other, "target_group_dn"),
    ("TEMPLATE_DN", VarKind::Other, "template_dn"),
    ("TARGET_OU", VarKind::Other, "target_ou"),
    ("EXTRA_SID", VarKind::Other, "extra_sid"),
    // target
    ("TARGET_FQDN", VarKind::Fqdn, "target_fqdn"),
    ("TARGET_HOST", VarKind::Host, "target_host"),
    ("TARGET_IP", VarKind::Ip, "target_ip"),
    ("TARGET", VarKind::Host, "target"),
    ("SUBNET", VarKind::Other, "subnet"),
    ("SMS_IP", VarKind::Ip, "sms_ip"),
    ("BHCE_IP", VarKind::Ip, "bhce_ip"),
    ("SERVER_IP", VarKind::Ip, "server_ip"),
    ("IP", VarKind::Ip, "target_ip"),
    // attacker side
    ("ATTACKER_IP", VarKind::LocalIp, "attacker_ip"),
    ("LISTENER_IP", VarKind::LocalIp, "attacker_ip"),
    ("LHOST_IP", VarKind::LocalIp, "attacker_ip"),
    ("LHOST", VarKind::LocalIp, "attacker_ip"),
    ("LPORT", VarKind::Port, "lport"),
    ("PORT", VarKind::Port, "port"),
    // wireless / net
    ("INTERFACE", VarKind::Iface, "iface"),
    ("BSSID", VarKind::Other, "bssid"),
    ("ESSID", VarKind::Other, "essid"),
    ("CHANNEL", VarKind::Other, "channel"),
    // misc
    ("SHARE", VarKind::Other, "share"),
    ("WORDLIST", VarKind::File, "wordlist"),
    ("HASHES_FILE", VarKind::File, "hashes_file"),
    ("PROFILE_NAME", VarKind::Other, "profile_name"),
    ("REQUEST_ID", VarKind::Other, "request_id"),
    ("STORAGE_ACCOUNT", VarKind::Other, "storage_account"),
    ("REFRESH_TOKEN", VarKind::Other, "refresh_token"),
    ("BASE64_BLOB", VarKind::Other, "base64_blob"),
    ("DBNAME", VarKind::Other, "dbname"),
    ("APPURL", VarKind::Other, "appurl"),
    ("OUTFILE", VarKind::File, "outfile"),
];

/// Tokens that are genuine placeholders in some commands and ordinary literals
/// in others (`FILE:` ccache prefixes, `KRB5CCNAME`, ffuf's `FUZZ`, registry
/// paths). Only promoted to a slot when a contextual flag rule vouches for them.
const AMBIGUOUS: &[(&str, VarKind, &str)] = &[
    ("FILE", VarKind::File, "file"),
    ("KEY", VarKind::File, "key"),
    ("CERT", VarKind::File, "cert"),
    ("TICKET", VarKind::File, "ticket"),
    ("SERVICE", VarKind::Other, "service"),
    ("QUERY", VarKind::Other, "query"),
    ("ACTION", VarKind::Other, "action"),
    ("PAYLOAD", VarKind::File, "payload"),
    ("PATH", VarKind::File, "path"),
];

/// Contextual rules: an option flag (or `NAME=` assignment) plus the value that
/// follows it. Drives both slot detection in the corpus and value harvesting
/// from shell history. `(flag, kind, canon)`.
const FLAG_RULES: &[(&str, VarKind, &str)] = &[
    ("-u", VarKind::User, "user"),
    ("--user", VarKind::User, "user"),
    ("-user", VarKind::User, "user"),
    ("--username", VarKind::User, "user"),
    ("-username", VarKind::User, "user"),
    ("-p", VarKind::Pass, "pass"),
    ("--password", VarKind::Pass, "pass"),
    ("-password", VarKind::Pass, "pass"),
    ("-H", VarKind::Hash, "hash"),
    ("--hashes", VarKind::Hash, "hash"),
    ("-hashes", VarKind::Hash, "hash"),
    ("--hash", VarKind::Hash, "hash"),
    ("-nthash", VarKind::Hash, "hash"),
    ("--nthash", VarKind::Hash, "hash"),
    ("-d", VarKind::Domain, "domain"),
    ("--domain", VarKind::Domain, "domain"),
    ("-domain", VarKind::Domain, "domain"),
    ("--dc-ip", VarKind::Ip, "dc_ip"),
    ("-dc-ip", VarKind::Ip, "dc_ip"),
    ("-ns", VarKind::Ip, "dc_ip"),
    ("--dc-host", VarKind::Fqdn, "dc_fqdn"),
    ("-dc-host", VarKind::Fqdn, "dc_fqdn"),
    ("-dc", VarKind::Fqdn, "dc_fqdn"),
    ("--dc", VarKind::Ip, "dc_ip"),
    ("--dnsdomain", VarKind::Domain, "domain"),
    ("-ca", VarKind::Other, "ca_name"),
    ("-target-ip", VarKind::Ip, "target_ip"),
    ("--target-ip", VarKind::Ip, "target_ip"),
    ("-w", VarKind::Pass, "pass"),
    ("-key", VarKind::File, "key"),
    ("-crt", VarKind::File, "cert"),
    ("-cert", VarKind::File, "cert"),
    ("-I", VarKind::Iface, "iface"),
    ("-i", VarKind::Iface, "iface"),
    ("--interface", VarKind::Iface, "iface"),
];

/// Flags that mean something different depending on the tool. `None` means the
/// flag carries no fillable value for that tool at all (msfvenom's `-p` is a
/// payload spec, curl's `-d` is a request body).
const TOOL_FLAG_OVERRIDES: &[(&str, &str, Option<(VarKind, &str)>)] = &[
    ("msfvenom", "-p", None),
    ("msfvenom", "--payload", None),
    ("ffuf", "-w", Some((VarKind::File, "wordlist"))),
    ("wfuzz", "-w", Some((VarKind::File, "wordlist"))),
    ("gobuster", "-w", Some((VarKind::File, "wordlist"))),
    ("feroxbuster", "-w", Some((VarKind::File, "wordlist"))),
    ("dirb", "-w", None),
    ("hashcat", "-w", None),
    ("john", "-w", Some((VarKind::File, "wordlist"))),
    ("hydra", "-p", Some((VarKind::Pass, "pass"))),
    ("curl", "-d", None),
    ("curl", "-H", None),
    ("aircrack-ng", "-w", Some((VarKind::File, "wordlist"))),
    // For a web fuzzer `-u` is the URL, not a username.
    ("ffuf", "-u", Some((VarKind::Other, "url"))),
    ("wfuzz", "-u", Some((VarKind::Other, "url"))),
    ("gobuster", "-u", Some((VarKind::Other, "url"))),
    ("feroxbuster", "-u", Some((VarKind::Other, "url"))),
    ("dirsearch", "-u", Some((VarKind::Other, "url"))),
    ("nikto", "-u", Some((VarKind::Other, "url"))),
    ("sqlmap", "-u", Some((VarKind::Other, "url"))),
    ("wpscan", "-u", Some((VarKind::Other, "url"))),
];

/// The program names invoked by a command — one per pipeline segment, so
/// `cat urls | ffuf -w …` still counts as ffuf. Matching flag overrides against
/// these rather than the raw text keeps a DirBuster wordlist *path* from being
/// read as an invocation of `dirb`.
fn tool_names(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut want = true;
    for t in tokens(cmd) {
        if t.sep {
            want = true;
            continue;
        }
        if !want {
            continue;
        }
        let raw = &cmd[t.raw_start..t.raw_end];
        if PROG_PREFIX.contains(&raw) {
            continue;
        }
        want = false;
        let base = raw.rsplit('/').next().unwrap_or(raw);
        let base = base.trim_end_matches(".exe").trim_end_matches(".py");
        if !base.is_empty() {
            out.push(base.to_ascii_lowercase());
        }
    }
    out
}

/// Resolve a flag against the tool actually being invoked.
/// `Some(None)` = this tool says the flag has no fillable value.
fn tool_override(cmd: &str, flag: &str) -> Option<Option<(VarKind, String)>> {
    let names = tool_names(cmd);
    TOOL_FLAG_OVERRIDES
        .iter()
        .find(|(tool, f, _)| *f == flag && names.iter().any(|n| n.contains(tool)))
        .map(|(_, _, o)| o.map(|(k, c)| (k, c.to_string())))
}

/// ffuf and wfuzz declare their own fuzzing keywords as `-w path:KEYWORD`.
/// Those are part of the tool's syntax, not blanks to fill.
fn tool_keywords(cmd: &str) -> Vec<String> {
    let lower = cmd.to_lowercase();
    if !(lower.contains("ffuf") || lower.contains("wfuzz")) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for m in Regex::new(r"-w\s+'?[^\s':]+:([A-Z][A-Z0-9_]*)")
        .unwrap()
        .captures_iter(cmd)
    {
        out.push(m[1].to_string());
    }
    // FUZZ is ffuf's implicit default keyword.
    out.push("FUZZ".to_string());
    out
}

/// `NAME=VALUE` assignments (msfvenom / nxc module options / sliver profiles).
/// Only the right-hand side is a slot; the option name never is.
const ASSIGN_RULES: &[(&str, VarKind, &str)] = &[
    ("LHOST", VarKind::LocalIp, "attacker_ip"),
    ("RHOST", VarKind::Ip, "target_ip"),
    ("LPORT", VarKind::Port, "lport"),
    ("RPORT", VarKind::Port, "port"),
    ("LISTENER", VarKind::LocalIp, "attacker_ip"),
    ("SERVER", VarKind::LocalIp, "attacker_ip"),
    ("USER", VarKind::User, "target_user"),
];

/// Underscored all-caps tokens that are constants, not blanks: SQL functions
/// and schema columns, registry types and hives, linker env vars, UAC flags.
/// Everything else matching that shape in this corpus is a placeholder.
const NOT_PLACEHOLDERS: &[&str] = &[
    "GROUP_CONCAT", "INFORMATION_SCHEMA", "IS_ROLEMEMBER", "IS_SRVROLEMEMBER", "LOAD_FILE",
    "SINGLE_CLOB", "SYSTEM_USER", "TO_CHAR", "UTL_INADDR", "TABLE_NAME", "TABLE_SCHEMA",
    "COLUMN_NAME", "SCHEMA_NAME", "OBJECT_NAME", "HKEY_LOCAL_MACHINE", "HKEY_CURRENT_USER",
    "HKEY_CLASSES_ROOT", "HKEY_USERS", "HKEY_CURRENT_CONFIG", "REG_DWORD", "REG_QWORD", "REG_SZ",
    "REG_BINARY", "REG_EXPAND_SZ", "REG_MULTI_SZ", "REG_NONE", "LD_LIBRARY_PATH", "LD_PRELOAD",
    "LD_RUN_PATH", "LS_COLORS", "PKG_CONFIG_PATH", "LDAPTLS_REQCERT", "LIBPROC_HIDE_KERNEL",
    "LIBGSSAPI_IMPL", "LIBGSSAPI_PREFIX", "IDENTITY_ENDPOINT", "IDENTITY_HEADER", "DATE_LOCAL",
    "UTC_TIME", "READ_ONLY", "RC4_HMAC_MD5", "EAP_TYPE_TLS", "PROTOCOL_TLS_SERVER",
    "DONT_REQ_PREAUTH", "PASSWD_NOTREQD", "TRUSTED_FOR_DELEGATION", "NOT_DELEGATED",
    "TRUSTED_TO_AUTH_FOR_DELEGATION", "DONT_EXPIRE_PASSWORD", "SMARTCARD_REQUIRED",
    "ENCRYPTED_TEXT_PWD_ALLOWED", "USE_DES_KEY_ONLY", "HOMEDIR_REQUIRED", "NORMAL_ACCOUNT",
    "SERVER_TRUST_ACCOUNT", "WORKSTATION_TRUST_ACCOUNT", "INTERDOMAIN_TRUST_ACCOUNT",
    "LOGON_INTERACTIVE", "IF_ENFORCEENCRYPTICERTREQUEST",
];

/// IPs that mean something specific and must never become a blank.
const IP_KEEP: &[&str] = &[
    "127.0.0.1",
    "0.0.0.0",
    "255.255.255.255",
    "169.254.169.254",
    "8.8.8.8",
    "1.1.1.1",
    "224.0.0.1",
];

/// TLDs that mark a domain as lab scenery rather than a real service.
const LAB_TLDS: &[&str] = &["htb", "local", "vl", "lab", "ad", "corp", "thm", "hack"];

/// Env vars worth reading. Deliberately excludes bare `USER`/`USERNAME` — those
/// hold the *local* account name, which is a wrong and confusing default here.
const ENV_RULES: &[(&str, &str)] = &[
    ("RHOST", "target_ip"),
    ("RPORT", "port"),
    ("LHOST", "attacker_ip"),
    ("LPORT", "lport"),
    ("TARGET", "target"),
    ("DOMAIN", "domain"),
    ("DC_IP", "dc_ip"),
    ("RUSER", "user"),
    ("TARGET_USER", "target_user"),
];

// ---------------------------------------------------------------- regexes

fn canon_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Longest-first so TARGET_IP wins over TARGET, DC_HOSTNAME over DC_HOST.
        let mut toks: Vec<&str> = CANON
            .iter()
            .map(|(t, _, _)| *t)
            .chain(AMBIGUOUS.iter().map(|(t, _, _)| *t))
            .collect();
        toks.sort_by_key(|t| std::cmp::Reverse(t.len()));
        Regex::new(&format!("({})", toks.join("|"))).unwrap()
    })
}

fn angle_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<([A-Za-z][A-Za-z0-9_ .-]{1,40})>").unwrap())
}

fn ipv4_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(?:\d{1,3}\.){3}\d{1,3}\b").unwrap())
}

fn domain_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(?:[A-Za-z0-9_-]+\.)+[A-Za-z]{2,24}\b").unwrap())
}

/// An all-caps token with an underscore in it — the corpus's de-facto
/// convention for a blank it never bothered to add to the allowlist.
fn generic_ph_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+\b").unwrap())
}

/// Read the kind off the placeholder's own name, so `EXCHANGE_FQDN` reaches
/// /etc/hosts and `TUN_IP` reaches the tunnel address.
fn kind_from_name(t: &str) -> VarKind {
    let local = ["ATTACKER", "LHOST", "LISTENER", "TUN", "LOCAL", "OUR", "PIVOT", "SRVHOST"];
    if t.ends_with("_IP") || t.ends_with("_IPV6") || t.ends_with("_ADDR") {
        return if local.iter().any(|p| t.starts_with(p)) {
            VarKind::LocalIp
        } else {
            VarKind::Ip
        };
    }
    if t.ends_with("_FQDN") {
        return VarKind::Fqdn;
    }
    if t.ends_with("_HOST") || t.ends_with("_HOSTNAME") || t.ends_with("_SERVER") {
        return VarKind::Host;
    }
    if t.ends_with("_DOMAIN") || t.starts_with("DOMAIN_") {
        return VarKind::Domain;
    }
    if t.ends_with("_USER") || t.ends_with("_USERNAME") {
        return VarKind::User;
    }
    if t.ends_with("_PASS") || t.ends_with("_PASSWORD") {
        return VarKind::Pass;
    }
    if t.ends_with("_HASH") {
        return VarKind::Hash;
    }
    if t.ends_with("_PORT") {
        return VarKind::Port;
    }
    if t.ends_with("_FILE") || t.ends_with("_PATH") || t.ends_with("_LIST") || t.ends_with("_DIR") {
        return VarKind::File;
    }
    VarKind::Other
}

fn labhost_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(?:DC|SQL|WS|SRV|WEB|FS|MS)\d{2}\b").unwrap())
}

/// `DOMAIN/USER:PASSWORD@TARGET` — the impacket positional form.
fn impacket_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"([A-Za-z0-9_.-]+)/([A-Za-z0-9_.$-]+)(?::([^\s'@]+))?@([A-Za-z0-9_.-]+)")
            .unwrap()
    })
}

/// A bare token following a recognised flag: `-u jdoe`, `-p 'hunter2'`.
fn flag_value_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let mut flags: Vec<&str> = FLAG_RULES.iter().map(|(f, _, _)| *f).collect();
        flags.sort_by_key(|f| std::cmp::Reverse(f.len()));
        let alt = flags
            .iter()
            .map(|f| regex::escape(f))
            .collect::<Vec<_>>()
            .join("|");
        // group 1 = flag, 2 = single-quoted value, 3 = double-quoted, 4 = bare
        Regex::new(&format!(
            r#"(?:^|\s)({alt})[ =]+(?:'([^']*)'|"([^"]*)"|([^\s'";|&]+))"#
        ))
        .unwrap()
    })
}

fn assign_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let alt = ASSIGN_RULES
            .iter()
            .map(|(n, _, _)| *n)
            .collect::<Vec<_>>()
            .join("|");
        Regex::new(&format!(
            r#"\b({alt})=(?:'([^']*)'|"([^"]*)"|([^\s'";|&,]+))"#
        ))
        .unwrap()
    })
}

// ---------------------------------------------------------------- model

#[derive(Debug, Clone)]
pub struct Slot {
    pub start: usize,
    pub end: usize,
    pub field: usize,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub label: String,
    pub canon: String,
    pub kind: VarKind,
    pub value: String,
    /// Byte index into `value`.
    pub cursor: usize,
    pub origin: Origin,
    pub suggestions: Vec<(String, Origin)>,
    pub sugg_idx: usize,
    /// The text the slot originally held (a literal, or the placeholder token).
    pub literal: String,
    /// Set once the user types into the field, so a target switch leaves it be.
    pub edited: bool,
    /// Whether to read from / write to the sticky store under `canon`.
    pub sticky: bool,
}

#[derive(Debug, Clone)]
pub struct HostTarget {
    pub ip: String,
    pub fqdn: String,
    pub short: String,
    pub domain: String,
}

impl HostTarget {
    pub fn display(&self) -> &str {
        if !self.fqdn.is_empty() {
            &self.fqdn
        } else if !self.short.is_empty() {
            &self.short
        } else {
            &self.ip
        }
    }
}

pub struct FillState {
    pub title: String,
    pub cmd: String,
    pub slots: Vec<Slot>,
    pub fields: Vec<Field>,
    pub cur: usize,
    pub targets: Vec<HostTarget>,
    pub target_idx: usize,
    pub field_scroll: usize,
}

// ---------------------------------------------------------------- detection

/// True when `cmd[..pos]` / `cmd[end..]` make this a standalone token rather
/// than part of a longer identifier. The `regex` crate has no lookaround, so we
/// check the neighbouring bytes by hand.
fn boundary_ok(cmd: &str, start: usize, end: usize) -> bool {
    let b = cmd.as_bytes();
    let before_ok = start == 0 || {
        let c = b[start - 1] as char;
        // `/` is deliberately absent: `DOMAIN/ADMIN_USER`, `cifs/TARGET_FQDN`
        // and `ldap://DC_IP` all put a real placeholder right after a slash.
        !(c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '%' | '<' | '{' | '.' | '-'))
    };
    let after_ok = end >= b.len() || {
        let c = b[end] as char;
        // `-` mirrors the check above: `TARGET-URL` is one token, and filling
        // just the `TARGET` half of it would corrupt the command.
        !(c.is_ascii_alphanumeric() || matches!(c, '_' | '>' | '}' | '%' | '-'))
    };
    before_ok && after_ok
}

/// The LHS of `NAME=` is an option name (nxc's `EXCLUDE_DIR=`, `ALWAYS=`), never
/// a value to fill in.
fn is_assign_lhs(cmd: &str, end: usize) -> bool {
    cmd.as_bytes().get(end) == Some(&b'=')
}

struct Raw {
    start: usize,
    end: usize,
    label: String,
    canon: String,
    kind: VarKind,
    tier: u8,
    /// Whether the value is worth remembering across commands. False for
    /// Tier-4 positionals, whose canon (`arg`, `file`) means nothing outside
    /// the one command they came from.
    sticky: bool,
}

fn lookup(token: &str) -> Option<(VarKind, &'static str)> {
    CANON
        .iter()
        .chain(AMBIGUOUS.iter())
        .find(|(t, _, _)| *t == token)
        .map(|(_, k, c)| (*k, *c))
}

fn is_ambiguous(token: &str) -> bool {
    AMBIGUOUS.iter().any(|(t, _, _)| *t == token)
}

fn iface_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(?:eth|en|ens|eno|enp|wlan|wlp|wl|tun|utun|tap|ppp|mon|lo|at|hwsim|br|docker)[0-9a-z]*(?:mon)?$")
            .unwrap()
    })
}

fn is_ipv4(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 4 && parts.iter().all(|p| p.parse::<u8>().is_ok())
}

fn is_hostname(v: &str) -> bool {
    !v.is_empty()
        && v.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '$'))
}

/// Does this value look like the kind its flag claims? Without this gate the
/// flag rules swallow curl headers (`-H 'Content-Type: …'`), request bodies
/// (`-d 'user=admin&pass=FUZZ'`), thread counts (`-t 20`) and shell redirects.
fn plausible(kind: VarKind, v: &str) -> bool {
    if v.is_empty() || v.len() > 128 || v.chars().any(char::is_whitespace) {
        return false;
    }
    if v.contains(['>', '<', '|', '{', '}', '&', '\\', '*', '(', ')']) {
        return false;
    }
    // A canonical placeholder token is always fine, whatever the flag says.
    if lookup(v.trim_matches('\'').trim_matches('"')).is_some() {
        return true;
    }
    match kind {
        VarKind::Ip => is_ipv4(v) || is_hostname(v),
        VarKind::Fqdn | VarKind::Host | VarKind::Domain => {
            is_hostname(v) && !v.contains("://") && !v.contains('=')
        }
        VarKind::User => !v.contains("://") && !v.contains(['=', ':', '/']) && v.len() <= 64,
        VarKind::Pass => !v.contains("://") && !v.contains('/') && v.len() <= 64,
        VarKind::Hash => {
            v.len() >= 16 && v.chars().all(|c| c.is_ascii_hexdigit() || c == ':')
        }
        VarKind::Port => v.len() <= 5 && v.chars().all(|c| c.is_ascii_digit()),
        VarKind::Iface => iface_re().is_match(v),
        VarKind::File => !v.contains("://") && !v.contains('='),
        VarKind::LocalIp => is_ipv4(v) || is_hostname(v),
        VarKind::Other => true,
    }
}

/// Find every fillable span in `cmd`, then collapse repeats of the same canon
/// into one field. Returns `(fields, slots)`; slots are sorted by start offset.
// ---------------------------------------------------------------- tier 4

/// Words that stand in front of the real program, so the *next* word is still
/// the command name rather than an argument.
const PROG_PREFIX: &[&str] = &[
    "sudo",
    "doas",
    "env",
    "time",
    "nohup",
    "unbuffer",
    "stdbuf",
    "proxychains",
    "proxychains4",
];

/// Flags that take no value. Without these, a boolean flag would swallow the
/// positional that follows it and mislabel the field.
const BOOLEAN_FLAGS: &[&str] = &[
    "-v", "-vv", "-vvv", "-q", "-h", "-k", "-n", "-A", "-O", "-Pn", "-sV", "-sC", "-sS", "-sU",
    "-sT", "-sn", "-6", "-4", "-a", "--help", "--version", "--verbose", "--quiet", "--debug",
    "--force", "--dump", "--ssl", "--no-pass", "-no-pass", "--local-auth", "--kdcHost",
    "--continue-on-success", "--shares", "--users", "--groups", "--sessions", "--disks",
    "--pass-pol", "--loggedon-users", "--json", "--csv", "--no-color", "--recursive", "--self",
    "--enabled", "--dc-list", "--admin-count", "--stealth", "--all", "--dns-tcp",
];

/// One shell word. `raw_start..raw_end` is the whole word; `start..end` is its
/// value with a wrapping pair of quotes removed.
struct Tok {
    raw_start: usize,
    raw_end: usize,
    start: usize,
    end: usize,
    sep: bool,
}

/// Split a command into words plus separators (`|`, `;`, `&&`, newline).
/// Quotes hold a word together; a trailing `\` joins the next line.
fn tokens(cmd: &str) -> Vec<Tok> {
    let b = cmd.as_bytes();
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i] as char;
        if c == '\\' && i + 1 < b.len() && b[i + 1] == b'\n' {
            i += 2;
            continue;
        }
        if c.is_ascii_whitespace() {
            if c == '\n' {
                out.push(Tok { raw_start: i, raw_end: i + 1, start: i, end: i + 1, sep: true });
            }
            i += 1;
            continue;
        }
        if matches!(c, '|' | ';' | '&') {
            let mut j = i + 1;
            while j < b.len() && matches!(b[j] as char, '|' | ';' | '&') {
                j += 1;
            }
            out.push(Tok { raw_start: i, raw_end: j, start: i, end: j, sep: true });
            i = j;
            continue;
        }
        let start = i;
        let mut quote: Option<u8> = None;
        while i < b.len() {
            let ch = b[i];
            match quote {
                Some(q) => {
                    if ch == q {
                        quote = None;
                    }
                    i += 1;
                }
                None => {
                    if ch == b'\'' || ch == b'"' {
                        quote = Some(ch);
                        i += 1;
                    } else if ch == b'\\' && i + 1 < b.len() && b[i + 1] == b'\n' {
                        break;
                    } else if (ch as char).is_ascii_whitespace()
                        || matches!(ch, b'|' | b';' | b'&')
                    {
                        break;
                    } else if ch == b'\\' && i + 1 < b.len() {
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
        }
        let (s, e) = unquote_span(cmd, start, i);
        out.push(Tok { raw_start: start, raw_end: i, start: s, end: e, sep: false });
    }
    out
}

/// Narrow a span past one wrapping pair of quotes, so the user edits the value
/// and the quoting survives untouched.
fn unquote_span(cmd: &str, s: usize, e: usize) -> (usize, usize) {
    let b = cmd.as_bytes();
    if e > s + 1 {
        let f = b[s];
        if (f == b'\'' || f == b'"') && b[e - 1] == f && !cmd[s + 1..e - 1].contains(f as char) {
            return (s + 1, e - 1);
        }
    }
    (s, e)
}

/// Words that only ever show up in English prose. Some corpus entries are
/// written instructions rather than commands ("Proceed to navigate to the File
/// menu…"), and splitting those into arguments yields dozens of junk fields.
const PROSE_WORDS: &[&str] = &[
    "the", "and", "that", "this", "with", "into", "will", "your", "you", "our", "are", "was",
    "were", "have", "has", "been", "which", "would", "should", "could", "also", "they", "their",
    "them", "its", "must", "can", "may", "upon", "ensure", "thereby", "via", "there", "these",
    "those", "about", "after", "before", "because", "however",
];

/// Leading keywords that mean the entry is a query or a protocol transcript,
/// not a shell command line. Matched case-sensitively: the corpus writes SQL
/// and HTTP verbs in caps, while `use` and `get` are real shell verbs.
const NON_SHELL_HEADS: &[&str] = &[
    "SELECT", "INSERT", "UPDATE", "DELETE", "CREATE", "ALTER", "DROP", "EXEC", "EXECUTE", "USE",
    "WITH", "DECLARE", "GRANT", "REVOKE", "BEGIN", "UNION", "GET", "POST", "PUT", "HEAD", "PATCH",
    "OPTIONS", "EHLO", "HELO", "MAIL", "RCPT", "DATA", "QUIT", "QUERY", "MUTATION", "FRAGMENT",
];

/// Whether Tier 4 should touch this entry at all. Only bare, unquoted words
/// count towards the prose score, so `-x 'net user the thing'` stays a command.
fn commandish(cmd: &str) -> bool {
    let head = cmd.trim_start();
    if head.starts_with(['{', '<', '[', '#']) {
        return false;
    }
    let first = head.split(|c: char| c.is_whitespace() || c == '(').next().unwrap_or("");
    if NON_SHELL_HEADS.contains(&first)
        || matches!(
            first.to_ascii_lowercase().as_str(),
            "query" | "mutation" | "fragment" | "subscription"
        )
    {
        return false;
    }
    let mut prose = 0usize;
    // GUI click paths ("Project > Manage NuGetPackages > Settings"), and the
    // Title Case that only appears in written instructions. A cmdlet like
    // `Get-ADUser` is not Title Case by this test — it has a hyphen.
    let mut arrows = 0usize;
    let mut titles = 0usize;
    for t in tokens(cmd) {
        if t.sep {
            continue;
        }
        let v = &cmd[t.start..t.end];
        if v.chars().any(char::is_whitespace) {
            continue;
        }
        if v == ">" {
            arrows += 1;
        }
        let mut cs = v.chars();
        if v.len() >= 2
            && cs.next().is_some_and(|c| c.is_ascii_uppercase())
            && cs.all(|c| c.is_ascii_lowercase())
        {
            titles += 1;
        }
        let w = v.trim_matches(|c: char| !c.is_ascii_alphabetic()).to_ascii_lowercase();
        if PROSE_WORDS.contains(&w.as_str()) {
            prose += 1;
        }
        if prose >= 3 || arrows >= 2 || titles >= 4 {
            return false;
        }
    }
    true
}

/// Windows / xfreerdp switch syntax: `/dynamic-resolution`, `/u:USER`,
/// `/drive:.,linux`. Returns the offset of the `:` inside the switch, or
/// `Some(None)` when the switch carries no value. A second `/` or a `.` in the
/// name means it is a path, not a switch.
fn switch_split(raw: &str) -> Option<Option<usize>> {
    let rest = raw.strip_prefix('/')?;
    let name = rest.split(':').next().unwrap_or("");
    if name.is_empty()
        || !name.starts_with(|c: char| c.is_ascii_alphabetic())
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return None;
    }
    Some(rest.find(':'))
}

/// A real option, not a stray dash in prose (`-T5,-T4`, `--`, `-`).
fn flagish(raw: &str) -> bool {
    let name = raw.trim_start_matches('-');
    let name = name.split('=').next().unwrap_or("");
    !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

/// `--dc-ip` -> `DC_IP`. The flag is the only name a generic value has.
fn flag_label(flag: &str) -> String {
    flag.trim_start_matches('-').replace('-', "_").to_uppercase()
}

/// A word safe to turn into a slot: no shell expansion, no operators, and
/// something a person could actually retype.
fn arg_fillable(v: &str) -> bool {
    if v.is_empty() || v.len() > 200 || v.starts_with('#') {
        return false;
    }
    if v.chars().any(|c| {
        matches!(c, '$' | '`' | '*' | '(' | ')' | '{' | '}' | '|' | '&' | ';' | '<' | '>' | '\\')
    }) {
        return false;
    }
    // `0.0.0.0`, the cloud metadata address and friends mean something specific
    // wherever they appear, including inside a URL.
    if ipv4_re().find_iter(v).any(|m| IP_KEEP.contains(&m.as_str())) {
        return false;
    }
    v.chars().any(|c| c.is_ascii_alphanumeric())
}

/// A bare lowercase word before the first flag is a subcommand (`kerbrute
/// userenum`, `nxc smb`, `certipy req`), not a value.
fn subcommandish(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 24
        && v.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
        && v.chars().any(|c| c.is_ascii_alphabetic())
}

fn wordlistish(v: &str) -> bool {
    let l = v.to_lowercase();
    l.contains("wordlist") || l.contains("seclists") || l.contains("rockyou") || l.ends_with(".lst")
}

/// Guess what a bare value is, so suggestions and Ctrl+T can reach it.
fn infer_kind(v: &str) -> VarKind {
    if is_ipv4(v) {
        return VarKind::Ip;
    }
    if v.len() <= 5 && v.chars().all(|c| c.is_ascii_digit()) {
        return VarKind::Port;
    }
    if v.contains('/') || v.starts_with('~') || v.starts_with('.') {
        return VarKind::File;
    }
    let ext = v.rsplit('.').next().unwrap_or("");
    if v.contains('.')
        && matches!(
            ext,
            "txt" | "lst" | "list" | "csv" | "json" | "xml" | "ccache" | "kirbi" | "pem" | "key"
                | "crt" | "pfx" | "exe" | "dll" | "ps1" | "py" | "sh" | "zip" | "log"
        )
    {
        return VarKind::File;
    }
    VarKind::Other
}

/// An unlisted placeholder token — `USERS_FILE`, `TARGET_OU_DN`. The corpus
/// invents these freely, and the underscore is what separates them from a real
/// all-caps value like `ESC1` or `SYSTEM`.
fn placeholderish(v: &str) -> bool {
    v.len() >= 4
        && v.contains('_')
        && v.starts_with(|c: char| c.is_ascii_uppercase())
        && v.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// An option's value, named after the option.
fn push_arg(cmd: &str, raws: &mut Vec<Raw>, label: String, s: usize, e: usize) {
    let (s, e) = unquote_span(cmd, s, e);
    if s >= e || label.is_empty() {
        return;
    }
    let v = &cmd[s..e];
    if !arg_fillable(v) {
        return;
    }
    // A placeholder names itself better than the option that carries it.
    let label = if placeholderish(v) { v.to_string() } else { label };
    raws.push(Raw {
        start: s,
        end: e,
        canon: label.to_lowercase(),
        kind: infer_kind(v),
        label,
        tier: 4,
        sticky: true,
    });
}

/// A bare argument. It has no option to name it, so the label comes from the
/// shape of the value — and only a wordlist is worth remembering, since `arg`
/// and `file` mean nothing outside the command they came from.
fn push_positional(cmd: &str, raws: &mut Vec<Raw>, s: usize, e: usize) {
    let v = &cmd[s..e];
    if !arg_fillable(v) {
        return;
    }
    let kind = infer_kind(v);
    if placeholderish(v) {
        raws.push(Raw {
            start: s,
            end: e,
            label: v.to_string(),
            canon: v.to_lowercase(),
            kind,
            tier: 4,
            sticky: true,
        });
        return;
    }
    let (label, canon, sticky) = if wordlistish(v) {
        ("WORDLIST", "wordlist", false)
    } else if kind == VarKind::File {
        ("FILE", "file", false)
    } else {
        ("ARG", "arg", false)
    };
    raws.push(Raw {
        start: s,
        end: e,
        label: label.to_string(),
        canon: canon.to_string(),
        kind,
        tier: 4,
        sticky,
    });
}

/// Tier 4 — every remaining option value and positional argument. The tiers
/// above know what a value *means*; this one only knows that it is a value, so
/// the label is the option it follows and the default is the literal. It runs
/// last and loses every overlap, so it only fills the gaps.
fn tier4(cmd: &str, raws: &mut Vec<Raw>) {
    if !commandish(cmd) {
        return;
    }
    let toks = tokens(cmd);
    // 0 = still looking for the program, 1 = subcommands, 2 = arguments.
    let mut state = 0u8;
    let mut i = 0usize;
    while i < toks.len() {
        let t = &toks[i];
        if t.sep {
            state = 0;
            i += 1;
            continue;
        }
        let raw = &cmd[t.raw_start..t.raw_end];
        let val = &cmd[t.start..t.end];

        if state == 0 {
            if !PROG_PREFIX.contains(&raw) {
                state = 1;
            }
            i += 1;
            continue;
        }

        if let Some(colon) = switch_split(raw) {
            state = 2;
            if let Some(j) = colon {
                push_arg(
                    cmd,
                    raws,
                    flag_label(&raw[1..1 + j]),
                    t.raw_start + j + 2,
                    t.raw_end,
                );
            }
            i += 1;
            continue;
        }

        if raw.starts_with('-') && raw.len() > 1 {
            state = 2;
            if !flagish(raw) {
                i += 1;
                continue;
            }
            // `--long=VALUE`
            if let Some(eq) = raw.find('=') {
                if eq > 1 {
                    push_arg(
                        cmd,
                        raws,
                        flag_label(&raw[..eq]),
                        t.raw_start + eq + 1,
                        t.raw_end,
                    );
                    i += 1;
                    continue;
                }
            }
            // A boolean flag takes nothing, so the next word is a positional.
            if BOOLEAN_FLAGS.contains(&raw) {
                i += 1;
                continue;
            }
            // The tool says this flag's value is not fillable (a curl header, a
            // msfvenom payload spec) — swallow it rather than leaving it to be
            // picked up as a bare argument.
            if matches!(tool_override(cmd, raw), Some(None)) {
                i += if toks
                    .get(i + 1)
                    .is_some_and(|n| !n.sep && !cmd[n.raw_start..n.raw_end].starts_with('-'))
                {
                    2
                } else {
                    1
                };
                continue;
            }
            if let Some(n) = toks.get(i + 1) {
                let next = &cmd[n.raw_start..n.raw_end];
                if !n.sep && !next.starts_with('-') && switch_split(next).is_none() {
                    push_arg(cmd, raws, flag_label(raw), n.start, n.end);
                    i += 2;
                    continue;
                }
            }
            i += 1;
            continue;
        }

        if state == 1 && subcommandish(val) {
            i += 1;
            continue;
        }
        state = 2;

        // `NAME=VALUE` (msfvenom, module options): only the value is a slot.
        if let Some(eq) = val.find('=') {
            if eq > 0
                && val[..eq]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            {
                push_arg(cmd, raws, flag_label(&val[..eq]), t.start + eq + 1, t.end);
                i += 1;
                continue;
            }
        }
        push_positional(cmd, raws, t.start, t.end);
        i += 1;
    }
}

pub fn detect(cmd: &str) -> (Vec<Field>, Vec<Slot>) {
    let mut raws: Vec<Raw> = Vec::new();

    // Tier 0 — explicit <TOKEN>.
    for m in angle_re().captures_iter(cmd) {
        let whole = m.get(0).unwrap();
        let inner = m.get(1).unwrap().as_str();
        // Skip HTML/XML: <script>, </div>, <?xml ...>. Placeholders are upper.
        if !inner
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || matches!(c, '_' | ' ' | '-'))
        {
            continue;
        }
        let norm = inner.trim().replace([' ', '-'], "_");
        let (kind, canon) = lookup(&norm)
            .map(|(k, c)| (k, c.to_string()))
            .unwrap_or((VarKind::Other, norm.to_lowercase()));
        raws.push(Raw {
            start: whole.start(),
            end: whole.end(),
            label: norm,
            canon,
            kind,
            tier: 0,            sticky: true,
        });
    }

    // Tier 2a — contextual flags. Run before Tier 1 so an ambiguous token like
    // `KEY` in `-key KEY.key` gets vouched for, and so literal values after a
    // flag (`-u htb-student`) become fields too.
    for m in flag_value_re().captures_iter(cmd) {
        let flag = m.get(1).unwrap().as_str();
        let Some(val) = m.get(2).or(m.get(3)).or(m.get(4)) else {
            continue;
        };
        if val.as_str().is_empty() || val.as_str().starts_with('-') {
            continue;
        }
        let resolved = match tool_override(cmd, flag) {
            Some(None) => continue, // this tool's flag carries no fillable value
            Some(Some((k, c))) => Some((k, c)),
            None => FLAG_RULES
                .iter()
                .find(|(f, _, _)| *f == flag)
                .map(|(_, k, c)| (*k, c.to_string())),
        };
        let Some((mut kind, canon_owned)) = resolved else {
            continue;
        };
        let mut canon: &str = &canon_owned;
        // `-p 445` is a port, not a password — reclassify rather than reject.
        if kind == VarKind::Pass
            && !val.as_str().is_empty()
            && val.as_str().chars().all(|c| c.is_ascii_digit())
        {
            kind = VarKind::Port;
            canon = "port";
        }
        if !plausible(kind, val.as_str()) {
            continue;
        }
        // If the value is itself a canonical token, prefer that token's identity.
        let token = val.as_str().trim_matches('\'');
        let (kind, canon, label) = match lookup(token) {
            Some((k, c)) => (k, c.to_string(), token.to_string()),
            None => (kind, canon.to_string(), canon.to_uppercase()),
        };
        raws.push(Raw {
            start: val.start(),
            end: val.end(),
            label,
            canon,
            kind,
            tier: 2,            sticky: true,
        });
    }

    // Tier 2b — NAME=VALUE assignments.
    for m in assign_re().captures_iter(cmd) {
        let name = m.get(1).unwrap().as_str();
        let Some(val) = m.get(2).or(m.get(3)).or(m.get(4)) else {
            continue;
        };
        if val.as_str().is_empty() {
            continue;
        }
        let (kind, canon) = ASSIGN_RULES
            .iter()
            .find(|(n, _, _)| *n == name)
            .map(|(_, k, c)| (*k, *c))
            .unwrap();
        if !plausible(kind, val.as_str()) {
            continue;
        }
        let token = val.as_str().trim_matches('\'');
        let (kind, canon, label) = match lookup(token) {
            Some((k, c)) => (k, c.to_string(), token.to_string()),
            None => (kind, canon.to_string(), canon.to_uppercase()),
        };
        raws.push(Raw {
            start: val.start(),
            end: val.end(),
            label,
            canon,
            kind,
            tier: 2,            sticky: true,
        });
    }

    // Tier 2c — impacket positional DOMAIN/USER:PASSWORD@TARGET.
    for m in impacket_re().captures_iter(cmd) {
        let parts: [(usize, &str, VarKind, &str); 4] = [
            (1, "DOMAIN", VarKind::Domain, "domain"),
            (2, "USER", VarKind::User, "user"),
            (3, "PASSWORD", VarKind::Pass, "pass"),
            (4, "TARGET", VarKind::Host, "target"),
        ];
        for (g, dflabel, dfkind, dfcanon) in parts {
            let Some(cap) = m.get(g) else { continue };
            if cap.as_str().is_empty() {
                continue;
            }
            if !plausible(dfkind, cap.as_str()) {
                continue;
            }
            let (kind, canon, label) = match lookup(cap.as_str()) {
                Some((k, c)) => (k, c.to_string(), cap.as_str().to_string()),
                None => (dfkind, dfcanon.to_string(), dflabel.to_string()),
            };
            raws.push(Raw {
                start: cap.start(),
                end: cap.end(),
                label,
                canon,
                kind,
                tier: 2,                sticky: true,
            });
        }
    }

    // Tier 1 — canonical ALL_CAPS allowlist.
    let vouched: Vec<(usize, usize)> = raws.iter().map(|r| (r.start, r.end)).collect();
    for m in canon_re().find_iter(cmd) {
        let token = m.as_str();
        if !boundary_ok(cmd, m.start(), m.end()) || is_assign_lhs(cmd, m.end()) {
            continue;
        }
        // Ambiguous tokens need a Tier-2 rule to have already claimed this span.
        if is_ambiguous(token) && !vouched.iter().any(|(s, e)| *s <= m.start() && m.end() <= *e) {
            continue;
        }
        let (kind, canon) = lookup(token).unwrap();
        raws.push(Raw {
            start: m.start(),
            end: m.end(),
            label: token.to_string(),
            canon: canon.to_string(),
            kind,
            tier: 1,            sticky: true,
        });
    }

    // Tier 3 — literal lab values, pre-filled with the original so that
    // Enter-ing straight through reproduces the command byte for byte.
    for m in ipv4_re().find_iter(cmd) {
        if IP_KEEP.contains(&m.as_str()) {
            continue;
        }
        if m.as_str().split('.').any(|o| o.parse::<u16>().unwrap_or(999) > 255) {
            continue;
        }
        raws.push(Raw {
            start: m.start(),
            end: m.end(),
            label: "IP".into(),
            canon: "target_ip".into(),
            kind: VarKind::Ip,
            tier: 3,            sticky: true,
        });
    }
    for m in domain_re().find_iter(cmd) {
        let d = m.as_str();
        let tld = d.rsplit('.').next().unwrap_or("").to_lowercase();
        let is_lab = LAB_TLDS.contains(&tld.as_str())
            || d.to_lowercase().starts_with("inlanefreight.")
            || d.eq_ignore_ascii_case("example.com");
        if !is_lab {
            continue;
        }
        // Only the naked domain, not a hostname within it, if both matched.
        raws.push(Raw {
            start: m.start(),
            end: m.end(),
            label: if d.matches('.').count() > 1 { "FQDN".into() } else { "DOMAIN".into() },
            canon: if d.matches('.').count() > 1 { "target_fqdn".into() } else { "domain".into() },
            kind: if d.matches('.').count() > 1 { VarKind::Fqdn } else { VarKind::Domain },
            tier: 3,            sticky: true,
        });
    }
    for m in labhost_re().find_iter(cmd) {
        raws.push(Raw {
            start: m.start(),
            end: m.end(),
            label: "HOST".into(),
            canon: "target_host".into(),
            kind: VarKind::Host,
            tier: 3,            sticky: true,
        });
    }

    // Tier 3b — placeholders the allowlist has never heard of. The underscore
    // is what separates `API_KEY` and `USERS_FILE` from real all-caps text like
    // `SELECT`, `HKLM` or `ESC1`. Tier 3, so a flag rule that already knows what
    // the value means keeps it.
    for m in generic_ph_re().find_iter(cmd) {
        let token = m.as_str();
        if NOT_PLACEHOLDERS.contains(&token) || lookup(token).is_some() {
            continue;
        }
        if !boundary_ok(cmd, m.start(), m.end()) || is_assign_lhs(cmd, m.end()) {
            continue;
        }
        raws.push(Raw {
            start: m.start(),
            end: m.end(),
            label: token.to_string(),
            canon: token.to_lowercase(),
            kind: kind_from_name(token),
            tier: 3,
            sticky: true,
        });
    }

    tier4(cmd, &mut raws);

    // ffuf/wfuzz fuzzing keywords are tool syntax, not blanks — substituting
    // them would break the command.
    let keywords = tool_keywords(cmd);
    if !keywords.is_empty() {
        raws.retain(|r| {
            let text = cmd[r.start..r.end].trim_matches(['\'', '"']);
            !keywords.iter().any(|k| k == text)
        });
    }

    // Resolve overlaps: lowest tier wins, then longest span, then leftmost.
    raws.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then((b.end - b.start).cmp(&(a.end - a.start)))
            .then(a.start.cmp(&b.start))
    });
    let mut kept: Vec<Raw> = Vec::new();
    for r in raws {
        if kept.iter().any(|k| r.start < k.end && k.start < r.end) {
            continue;
        }
        kept.push(r);
    }
    kept.sort_by_key(|r| r.start);

    // One field per unique token, applied at every occurrence. Grouping is by
    // (canon, literal text): a placeholder repeated verbatim (`DOMAIN` … `DOMAIN`)
    // collapses to one field as intended, while two *different* literals that
    // happen to share a canon (two wordlists, two IPs) stay independent.
    let mut fields: Vec<Field> = Vec::new();
    let mut slots: Vec<Slot> = Vec::new();
    let mut by_canon: HashMap<(String, String), usize> = HashMap::new();
    let mut label_seen: HashMap<String, usize> = HashMap::new();
    for r in kept {
        let text = cmd[r.start..r.end].to_string();
        let idx = *by_canon.entry((r.canon.clone(), text)).or_insert_with(|| {
            // Disambiguate when the same label lands twice.
            let n = label_seen.entry(r.label.clone()).or_insert(0);
            *n += 1;
            let label = if *n == 1 {
                r.label.clone()
            } else {
                format!("{} {}", r.label, n)
            };
            fields.push(Field {
                label,
                canon: if *n == 1 {
                    r.canon.clone()
                } else {
                    format!("{}_{}", r.canon, n)
                },
                kind: r.kind,
                value: String::new(),
                cursor: 0,
                origin: Origin::Empty,
                suggestions: Vec::new(),
                sugg_idx: 0,
                literal: cmd[r.start..r.end].to_string(),
                edited: false,
                sticky: r.sticky,
            });
            fields.len() - 1
        });
        slots.push(Slot {
            start: r.start,
            end: r.end,
            field: idx,
        });
    }

    (fields, slots)
}

/// Apply the current field values to the original command.
pub fn render_filled(state: &FillState) -> String {
    let mut out = state.cmd.clone();
    // Right to left, so earlier byte offsets stay valid.
    for slot in state.slots.iter().rev() {
        let f = &state.fields[slot.field];
        let v = if f.value.is_empty() {
            f.literal.clone()
        } else {
            f.value.clone()
        };
        out.replace_range(slot.start..slot.end, &v);
    }
    out
}

// ---------------------------------------------------------------- context

pub struct VarContext {
    pub sticky: HashMap<String, String>,
    pub hosts: Vec<HostTarget>,
    /// canon -> values, newest first.
    pub history: HashMap<String, Vec<String>>,
    pub env: HashMap<String, String>,
    pub local_ip: Option<String>,
}

impl VarContext {
    /// Read everything off the machine. Called lazily on the first fill so
    /// startup stays instant.
    pub fn build(vars_path: &Path) -> Self {
        let sticky = load_sticky(vars_path);
        let history = harvest_history();
        let hosts = parse_hosts(&history_lines_cache());
        let mut env = HashMap::new();
        for (var, canon) in ENV_RULES {
            if let Ok(v) = std::env::var(var) {
                if !v.trim().is_empty() {
                    env.insert(canon.to_string(), v);
                }
            }
        }
        VarContext {
            sticky,
            hosts,
            history,
            env,
            local_ip: local_tunnel_ip(),
        }
    }

    /// Candidate values for a field, best first. The head becomes the default.
    pub fn suggest(&self, f: &Field, target: Option<&HostTarget>) -> Vec<(String, Origin)> {
        let mut out: Vec<(String, Origin)> = Vec::new();
        let push = |v: &str, o: Origin, out: &mut Vec<(String, Origin)>| {
            let v = v.trim();
            if v.is_empty() || out.iter().any(|(x, _)| x == v) {
                return;
            }
            out.push((v.to_string(), o));
        };

        // A bare positional has no name worth sharing across commands: the
        // corpus literal is what says *which kind* of value belongs there — a
        // username list, not whichever wordlist you last used. So it leads,
        // and the harvested alternatives stay one ^N away.
        if !f.sticky {
            push(&f.literal, Origin::Literal, &mut out);
        } else if let Some(v) = self.sticky.get(&f.canon) {
            push(v, Origin::Sticky, &mut out);
        }
        if let Some(t) = target {
            match f.kind {
                VarKind::Ip => push(&t.ip, Origin::Hosts, &mut out),
                VarKind::Fqdn => {
                    push(&t.fqdn, Origin::Hosts, &mut out);
                    push(&t.short, Origin::Hosts, &mut out);
                }
                VarKind::Host => {
                    push(&t.short, Origin::Hosts, &mut out);
                    push(&t.fqdn, Origin::Hosts, &mut out);
                    push(&t.ip, Origin::Hosts, &mut out);
                }
                VarKind::Domain => push(&t.domain, Origin::Hosts, &mut out),
                _ => {}
            }
        }
        if let Some(vals) = self.history.get(&f.canon) {
            for v in vals {
                push(v, Origin::History, &mut out);
            }
        }
        if let Some(v) = self.env.get(&f.canon) {
            push(v, Origin::Env, &mut out);
        }
        if f.kind == VarKind::LocalIp {
            if let Some(ip) = &self.local_ip {
                push(ip, Origin::LocalIp, &mut out);
            }
        }
        // The original text is only a sensible default when it was a concrete
        // literal, not a placeholder token we are meant to replace.
        if is_literalish(&f.literal) {
            push(&f.literal, Origin::Literal, &mut out);
        }
        out
    }

    /// Re-derive the host-shaped defaults after the active target changes.
    /// Fields the user has typed into are left alone.
    pub fn apply_target(&self, fields: &mut [Field], target: Option<&HostTarget>) {
        for f in fields.iter_mut() {
            if f.edited || !f.kind.from_target() {
                continue;
            }
            f.suggestions = self.suggest(f, target);
            let (v, o) = f
                .suggestions
                .first()
                .cloned()
                .unwrap_or((String::new(), Origin::Empty));
            f.cursor = v.len();
            f.value = v;
            f.origin = o;
        }
    }
}

/// A placeholder token (`DC_IP`, `<PORT>`) is not a usable default; a concrete
/// value (`10.129.5.5`, `htb-student`) is.
fn is_literalish(s: &str) -> bool {
    if s.is_empty() || s.starts_with('<') {
        return false;
    }
    let core = s.trim_matches('\'').trim_matches('"');
    if lookup(core).is_some() || placeholderish(core) {
        return false;
    }
    true
}

// ---------------------------------------------------------------- sticky store

pub fn load_sticky(path: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<HashMap<String, String>>(&s).ok())
        .unwrap_or_default()
}

pub fn save_sticky(path: &Path, map: &HashMap<String, String>) {
    if let Ok(s) = serde_json::to_string_pretty(map) {
        let _ = std::fs::write(path, s);
    }
}

// ---------------------------------------------------------------- /etc/hosts

/// Parse uncommented /etc/hosts lines into targets, ranked by how recently the
/// IP or any of its names showed up in shell history.
pub fn parse_hosts(history: &[String]) -> Vec<HostTarget> {
    let text = std::fs::read_to_string("/etc/hosts").unwrap_or_default();
    let mut out: Vec<HostTarget> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // Commented lines are stale boxes — ignore them entirely.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // A trailing `# note` is fine; strip it.
        let line = line.split('#').next().unwrap_or("").trim();
        let mut parts = line.split_whitespace();
        let Some(ip) = parts.next() else { continue };
        if ip.contains(':') || ip.starts_with("127.") || ip == "255.255.255.255" {
            continue;
        }
        let names: Vec<&str> = parts.filter(|n| *n != "localhost").collect();
        if names.is_empty() {
            continue;
        }
        let fqdn = names
            .iter()
            .max_by_key(|n| n.matches('.').count())
            .copied()
            .unwrap_or("");
        let short = names
            .iter()
            .find(|n| !n.contains('.'))
            .copied()
            .unwrap_or_else(|| fqdn.split('.').next().unwrap_or(""));
        // Prefer a domain that is explicitly listed as a suffix of the FQDN,
        // e.g. `RUN-SRV.tri.lab tri.lab RUN-SRV` -> `tri.lab`.
        let domain = names
            .iter()
            .find(|n| **n != fqdn && fqdn.to_lowercase().ends_with(&format!(".{}", n.to_lowercase())))
            .map(|s| s.to_string())
            .unwrap_or_else(|| match fqdn.split_once('.') {
                Some((_, rest)) if rest.contains('.') || !rest.is_empty() => rest.to_string(),
                _ => String::new(),
            });
        out.push(HostTarget {
            ip: ip.to_string(),
            fqdn: if fqdn.contains('.') { fqdn.to_string() } else { String::new() },
            short: short.to_string(),
            domain,
        });
    }

    // Rank: most recently referenced in shell history first, original order after.
    let mut recency: HashMap<usize, usize> = HashMap::new();
    for (age, line) in history.iter().enumerate() {
        let lower = line.to_lowercase();
        for (i, t) in out.iter().enumerate() {
            if recency.contains_key(&i) {
                continue;
            }
            let hit = lower.contains(&t.ip)
                || (!t.fqdn.is_empty() && lower.contains(&t.fqdn.to_lowercase()))
                || (!t.short.is_empty() && lower.contains(&t.short.to_lowercase()));
            if hit {
                recency.insert(i, age);
            }
        }
    }
    let mut indexed: Vec<(usize, HostTarget)> = out.into_iter().enumerate().collect();
    indexed.sort_by_key(|(i, _)| (recency.get(i).copied().unwrap_or(usize::MAX), *i));
    indexed.into_iter().map(|(_, t)| t).collect()
}

// ---------------------------------------------------------------- history

const HISTORY_SCAN: usize = 5000;

fn history_lines_cache() -> Vec<String> {
    static LINES: OnceLock<Vec<String>> = OnceLock::new();
    LINES.get_or_init(read_history_lines).clone()
}

/// Newest-first command lines from zsh and bash history.
fn read_history_lines() -> Vec<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    for name in [".zsh_history", ".bash_history"] {
        let path = Path::new(&home).join(name);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        // History files are not guaranteed UTF-8 (zsh metafies bytes >= 0x80).
        let text = String::from_utf8_lossy(&bytes);
        let mut cmds: Vec<String> = Vec::new();
        let mut pending = String::new();
        for raw in text.lines() {
            // zsh extended format: `: <epoch>:<elapsed>;<command>`
            let body = if raw.starts_with(": ") {
                raw.split_once(';').map(|(_, c)| c).unwrap_or(raw)
            } else {
                raw
            };
            if let Some(stripped) = body.strip_suffix('\\') {
                pending.push_str(stripped);
                pending.push(' ');
                continue;
            }
            let full = if pending.is_empty() {
                body.to_string()
            } else {
                let mut s = std::mem::take(&mut pending);
                s.push_str(body);
                s
            };
            if !full.trim().is_empty() {
                cmds.push(full);
            }
        }
        cmds.reverse();
        cmds.truncate(HISTORY_SCAN);
        out.extend(cmds);
    }
    out
}

/// Run the contextual flag rules over shell history to pull out the concrete
/// values actually used recently. Same table as detection, used in reverse.
fn harvest_history() -> HashMap<String, Vec<String>> {
    const KEEP_PER_VAR: usize = 6;
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let add = |canon: &str, v: &str, out: &mut HashMap<String, Vec<String>>| {
        let v = v.trim().trim_matches('\'').trim_matches('"');
        if v.is_empty() || v.len() > 200 || v.starts_with('-') || v.len() < 2 {
            return;
        }
        if !v.chars().any(|c| c.is_ascii_alphanumeric()) {
            return;
        }
        let slot = out.entry(canon.to_string()).or_default();
        if slot.len() >= KEEP_PER_VAR || slot.iter().any(|x| x == v) {
            return;
        }
        slot.push(v.to_string());
    };

    for line in history_lines_cache() {
        for m in flag_value_re().captures_iter(&line) {
            let flag = m.get(1).unwrap().as_str();
            let Some(val) = m.get(2).or(m.get(3)).or(m.get(4)) else {
                continue;
            };
            // Same tool overrides and plausibility gate as detection, so a
            // `bloodyAD -H RUN-SRV` host never lands in the hash bucket.
            let resolved = match tool_override(&line, flag) {
                Some(None) => continue,
                Some(Some((k, c))) => Some((k, c)),
                None => FLAG_RULES
                    .iter()
                    .find(|(f, _, _)| *f == flag)
                    .map(|(_, k, c)| (*k, c.to_string())),
            };
            let Some((mut kind, canon_owned)) = resolved else {
                continue;
            };
            let mut canon: &str = &canon_owned;
            if kind == VarKind::Pass && val.as_str().chars().all(|c| c.is_ascii_digit()) {
                kind = VarKind::Port;
                canon = "port";
            }
            if plausible(kind, val.as_str()) {
                add(canon, val.as_str(), &mut out);
            }
        }
        for m in assign_re().captures_iter(&line) {
            let name = m.get(1).unwrap().as_str();
            let Some(val) = m.get(2).or(m.get(3)).or(m.get(4)) else {
                continue;
            };
            if let Some((_, kind, canon)) = ASSIGN_RULES.iter().find(|(n, _, _)| *n == name) {
                if plausible(*kind, val.as_str()) {
                    add(canon, val.as_str(), &mut out);
                }
            }
        }
        for m in impacket_re().captures_iter(&line) {
            for (g, kind, canon) in [
                (1, VarKind::Domain, "domain"),
                (2, VarKind::User, "user"),
                (3, VarKind::Pass, "pass"),
                (4, VarKind::Host, "target"),
            ] {
                if let Some(c) = m.get(g) {
                    if plausible(kind, c.as_str()) {
                        add(canon, c.as_str(), &mut out);
                    }
                }
            }
        }
        // First positional after `nxc <proto>` / `netexec <proto>` is the target.
        let mut it = line.split_whitespace();
        while let Some(w) = it.next() {
            if w.ends_with("nxc") || w.ends_with("netexec") || w.ends_with("crackmapexec") {
                let _proto = it.next();
                if let Some(t) = it.next() {
                    if !t.starts_with('-') {
                        add("target", t, &mut out);
                    }
                }
                break;
            }
        }
    }
    out
}

// ---------------------------------------------------------------- local IP

/// The attacker-side address: prefer a VPN tunnel, fall back to the first
/// private RFC1918 address.
fn local_tunnel_ip() -> Option<String> {
    let output = if cfg!(target_os = "windows") {
        std::process::Command::new("ipconfig").output().ok()?
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("ifconfig").output().ok()?
    } else {
        std::process::Command::new("ip")
            .args(["-4", "-o", "addr"])
            .output()
            .ok()
            .or_else(|| std::process::Command::new("ifconfig").output().ok())?
    };
    let text = String::from_utf8_lossy(&output.stdout);

    let mut tunnel: Option<String> = None;
    let mut private: Option<String> = None;
    let mut iface = String::new();
    for line in text.lines() {
        if !line.starts_with(char::is_whitespace) && line.contains(':') {
            iface = line.split(':').next().unwrap_or("").trim().to_string();
        }
        // `ip -o addr` puts the interface in field 2 of each line.
        if let Some(f) = line.split_whitespace().nth(1) {
            if f.starts_with("tun") || f.starts_with("utun") || f.starts_with("ppp") {
                iface = f.to_string();
            }
        }
        let Some(m) = ipv4_re().find(line) else {
            continue;
        };
        let ip = m.as_str().to_string();
        if IP_KEEP.contains(&ip.as_str()) || ip.starts_with("127.") {
            continue;
        }
        let is_tun = iface.starts_with("tun") || iface.starts_with("utun") || iface.starts_with("ppp");
        if is_tun && tunnel.is_none() {
            tunnel = Some(ip);
        } else if private.is_none()
            && (ip.starts_with("10.") || ip.starts_with("192.168.") || ip.starts_with("172."))
        {
            private = Some(ip);
        }
    }
    tunnel.or(private)
}

// ---------------------------------------------------------------- audit

/// `f1nder --audit-vars`: run `detect()` over the whole corpus and report what
/// it found, so the corpus can be normalised towards explicit `<TOKEN>`s.
pub fn audit(entries: &[(String, String, String)], filter: Option<&str>) {
    // With a filter, dump the actual per-command detections — the view you want
    // when deciding whether a command needs its placeholders normalised.
    if let Some(pat) = filter {
        let pat = pat.to_lowercase();
        let mut shown = 0;
        for (_, title, cmd) in entries {
            if !cmd.to_lowercase().contains(&pat) && !title.to_lowercase().contains(&pat) {
                continue;
            }
            let (fields, slots) = detect(cmd);
            println!("\n\x1b[1m{title}\x1b[0m\n  {}", cmd.replace('\n', "\n  "));
            for (i, f) in fields.iter().enumerate() {
                let n = slots.iter().filter(|s| s.field == i).count();
                println!("    · {:<16} {:?}  x{n}  literal={:?}", f.label, f.kind, f.literal);
            }
            if fields.is_empty() {
                println!("    (no fields)");
            }
            shown += 1;
            if shown >= 40 {
                println!("\n… stopping at 40 matches.");
                break;
            }
        }
        return;
    }

    let mut by_file: HashMap<String, (usize, usize, HashMap<String, usize>)> = HashMap::new();
    let mut no_fields: Vec<&str> = Vec::new();
    let mut widest: Vec<(usize, &str)> = Vec::new();
    for (file, title, cmd) in entries {
        let (fields, _) = detect(cmd);
        widest.push((fields.len(), title));
        let slot = by_file.entry(file.clone()).or_default();
        slot.0 += 1;
        if fields.is_empty() {
            slot.1 += 1;
            no_fields.push(title);
        }
        for f in &fields {
            *slot.2.entry(f.label.clone()).or_insert(0) += 1;
        }
    }

    // The safety invariant: with nothing typed, every slot falls back to its
    // original text, so Enter-ing straight through must reproduce the command
    // byte for byte. Anything else means detection is corrupting commands.
    let mut broken: Vec<&str> = Vec::new();
    for (_, title, cmd) in entries {
        let (fields, slots) = detect(cmd);
        let st = FillState {
            title: String::new(),
            cmd: cmd.clone(),
            slots,
            fields,
            cur: 0,
            targets: vec![],
            target_idx: 0,
            field_scroll: 0,
        };
        if render_filled(&st) != *cmd {
            broken.push(title);
        }
    }

    let mut files: Vec<_> = by_file.into_iter().collect();
    files.sort_by_key(|(f, _)| f.clone());
    println!("{:<20} {:>7} {:>10} {:>8}", "file", "cmds", "no-fields", "fields");
    for (file, (total, none, labels)) in &files {
        let hits: usize = labels.values().sum();
        println!("{file:<20} {total:>7} {none:>10} {hits:>8}");
    }
    println!("\nTop detected fields:");
    let mut all: HashMap<String, usize> = HashMap::new();
    for (_, (_, _, labels)) in &files {
        for (k, v) in labels {
            *all.entry(k.clone()).or_insert(0) += v;
        }
    }
    let mut ranked: Vec<_> = all.into_iter().collect();
    ranked.sort_by_key(|(k, v)| (std::cmp::Reverse(*v), k.clone()));
    for (k, v) in ranked.iter().take(60) {
        println!("  {v:>5}  {k}");
    }
    // Every field is one more Enter before the command is on the clipboard,
    // so an outlier here usually means Tier 4 shredded a one-liner.
    widest.sort_by_key(|(n, t)| (std::cmp::Reverse(*n), *t));
    println!("\nMost fields in one command:");
    for (n, t) in widest.iter().take(10) {
        println!("  {n:>5}  {t}");
    }
    println!("\n{} command(s) with no detected fields.", no_fields.len());
    if broken.is_empty() {
        println!("round-trip: OK — all {} commands unchanged when left at defaults.", entries.len());
    } else {
        println!("round-trip: \x1b[31m{} command(s) CORRUPTED\x1b[0m:", broken.len());
        for t in broken.iter().take(20) {
            println!("  ! {t}");
        }
    }

    // What the machine currently offers as defaults.
    let ctx = VarContext::build(Path::new("/nonexistent-sticky"));
    println!("\n/etc/hosts targets (most recently used first):");
    for t in ctx.hosts.iter().take(8) {
        println!("  {:<16} fqdn={:<28} short={:<12} domain={}", t.ip, t.fqdn, t.short, t.domain);
    }
    println!("\nharvested from shell history:");
    let mut hist: Vec<_> = ctx.history.iter().collect();
    hist.sort_by_key(|(k, _)| (*k).clone());
    for (k, v) in hist {
        println!("  {k:<14} {}", v.join(", "));
    }
    println!("\nlocal tunnel IP: {:?}", ctx.local_ip);
    if !ctx.env.is_empty() {
        println!("env: {:?}", ctx.env);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(cmd: &str) -> Vec<String> {
        detect(cmd).0.into_iter().map(|f| f.label).collect()
    }

    /// Enter-ing through without typing must reproduce the original exactly.
    fn roundtrip(cmd: &str) -> String {
        let (fields, slots) = detect(cmd);
        let st = FillState {
            title: String::new(),
            cmd: cmd.to_string(),
            slots,
            fields,
            cur: 0,
            targets: vec![],
            target_idx: 0,
            field_scroll: 0,
        };
        render_filled(&st)
    }

    #[test]
    fn keywords_are_not_placeholders() {
        for cmd in [
            "SELECT * FROM users WHERE id=1 INTO OUTFILE '/tmp/x'",
            "reg query HKLM\\SYSTEM\\CurrentControlSet",
            "curl -X POST http://github.com/api -H 'Content-Type: application/json'",
            "<!DOCTYPE foo [<!ENTITY xxe SYSTEM \"file:///etc/passwd\">]>",
        ] {
            let l = labels(cmd);
            for bad in ["SELECT", "FROM", "WHERE", "HKLM", "POST", "ENTITY", "DOCTYPE"] {
                assert!(!l.contains(&bad.to_string()), "{cmd} -> {l:?} contains {bad}");
            }
        }
    }

    #[test]
    fn canonical_tokens() {
        let l = labels(
            "bloodhound-ce-python -u 'USER@DOMAIN' -p 'PASSWORD' -ns 'DC_IP' -d 'DOMAIN' -dc 'DC_FQDN'",
        );
        for want in ["USER", "DOMAIN", "PASSWORD", "DC_IP", "DC_FQDN"] {
            assert!(l.contains(&want.to_string()), "{l:?} missing {want}");
        }
    }

    #[test]
    fn repeated_token_is_one_field() {
        let (fields, slots) = detect("ldapsearch -H ldap://'DC_IP' -b \"DC=DOMAIN,DC=local\" -D 'USER@DOMAIN'");
        let domains = fields.iter().filter(|f| f.canon == "domain").count();
        assert_eq!(domains, 1, "{:?}", fields.iter().map(|f| &f.label).collect::<Vec<_>>());
        assert!(slots.iter().filter(|s| fields[s.field].canon == "domain").count() >= 2);
    }

    #[test]
    fn assignment_lhs_is_not_a_slot() {
        let (_, slots) = detect("nxc smb 'TARGET' -M spider_plus -o EXCLUDE_DIR=IPC$,print$");
        let cmd = "nxc smb 'TARGET' -M spider_plus -o EXCLUDE_DIR=IPC$,print$";
        for s in &slots {
            assert_ne!(&cmd[s.start..s.end], "EXCLUDE_DIR");
        }
    }

    #[test]
    fn literals_roundtrip_untouched() {
        for cmd in [
            "nxc smb 10.129.205.81 -u htb-student -p 'Password1' -d inlanefreight.local",
            "ssh root@10.10.14.3 -p 2222",
            "curl -s http://127.0.0.1:8080/ | jq .",
            "impacket-secretsdump INLANEFREIGHT.LOCAL/administrator:Pass@172.16.5.5",
        ] {
            assert_eq!(roundtrip(cmd), cmd, "roundtrip changed: {cmd}");
        }
    }

    #[test]
    fn placeholder_roundtrip_untouched() {
        let cmd = "certipy req -u 'USER@DOMAIN' -p 'PASSWORD' -dc-ip 'DC_IP' -ca 'CA_NAME'";
        assert_eq!(roundtrip(cmd), cmd);
    }

    #[test]
    fn safe_ips_are_kept() {
        let (_, slots) = detect("nc -lvnp 4444 -s 0.0.0.0 && curl 169.254.169.254/latest/meta-data");
        let cmd = "nc -lvnp 4444 -s 0.0.0.0 && curl 169.254.169.254/latest/meta-data";
        for s in &slots {
            let t = &cmd[s.start..s.end];
            assert!(t != "0.0.0.0" && t != "169.254.169.254", "templated {t}");
        }
    }

    /// The URL is an argument like any other, but `github.com` must never be
    /// read as the engagement domain.
    #[test]
    fn real_domains_are_not_blanks() {
        let l = labels("git clone https://github.com/SecureAuthCorp/impacket.git");
        assert_eq!(l, vec!["FILE".to_string()], "{l:?}");
    }

    #[test]
    fn tool_aware_flags() {
        // msfvenom -p is a payload spec, not a password.
        let l = labels("msfvenom -p windows/x64/shell_reverse_tcp LHOST='LHOST_IP' LPORT='PORT' -f exe");
        assert!(!l.contains(&"PASS".to_string()), "{l:?}");
        assert!(l.contains(&"LHOST_IP".to_string()) && l.contains(&"PORT".to_string()), "{l:?}");

        // ffuf -w is a wordlist, and FUZZ/keywords are tool syntax.
        let l = labels("ffuf -w ./custom.txt -u http://TARGET_IP/x.php -d \"user=admin&pass=FUZZ\"");
        assert!(l.contains(&"WORDLIST".to_string()), "{l:?}");
        assert!(!l.contains(&"FUZZ".to_string()) && !l.contains(&"PASS".to_string()), "{l:?}");

        // curl headers and request bodies are not fillable values; the URL is.
        let cmd = "curl -H \"Content-Type: application/json\" -d '{\"a\":1}' https://api.github.com/x";
        let l = labels(cmd);
        assert_eq!(l, vec!["FILE".to_string()], "{l:?}");

        // Thread counts and boolean flags are not values.
        let l = labels("gobuster dir -u http://x -t 20 -w /usr/share/wordlists/w.txt");
        assert!(!l.contains(&"TARGET".to_string()), "{l:?}");
    }

    /// `DOMAIN/USER` and `cifs/TARGET_FQDN` put a placeholder right after a
    /// slash — the token boundary check must not treat that as a path segment.
    #[test]
    fn placeholders_after_a_slash_are_found() {
        let l = labels("impacket-secretsdump 'DOMAIN/ADMIN_USER':'PASSWORD'@'DC_IP'");
        for want in ["DOMAIN", "ADMIN_USER", "PASSWORD", "DC_IP"] {
            assert!(l.contains(&want.to_string()), "{l:?} missing {want}");
        }
        assert!(labels("impacket-ticketer -spn cifs/'TARGET_FQDN' 'ADMIN_USER'")
            .contains(&"TARGET_FQDN".to_string()));
        assert!(labels("ldapsearch -H ldap://'DC_IP' -x").contains(&"DC_IP".to_string()));
    }

    #[test]
    fn distinct_literals_do_not_share_a_field() {
        let (fields, _) = detect("ffuf -w a.txt:USERS -w b.txt:PASSES -u http://TARGET_IP/");
        let wl: Vec<_> = fields.iter().filter(|f| f.canon.starts_with("wordlist")).collect();
        assert_eq!(wl.len(), 2, "{:?}", fields.iter().map(|f| &f.label).collect::<Vec<_>>());
        assert_ne!(wl[0].canon, wl[1].canon);
    }

    #[test]
    fn interface_flag_only_accepts_interfaces() {
        assert!(labels("sudo responder -I tun0 -v").contains(&"IFACE".to_string()));
        // `-i` on a file or a URL is not an interface.
        let l = labels("aircrack-ng -i HTB-01.csv");
        assert!(!l.contains(&"IFACE".to_string()), "{l:?}");
    }

    #[test]
    fn hosts_domain_derivation() {
        // Exercised via parse_hosts only when /etc/hosts is readable; the
        // suffix rule itself is what matters here.
        let t = HostTarget {
            ip: "10.1.201.128".into(),
            fqdn: "RUN-SRV.tri.lab".into(),
            short: "RUN-SRV".into(),
            domain: "tri.lab".into(),
        };
        assert_eq!(t.display(), "RUN-SRV.tri.lab");
    }

    // ------------------------------------------------------------ tier 4

    /// Every option carries a fillable value, named after the option.
    #[test]
    fn every_option_value_becomes_a_field() {
        let cmd = "ldapnomnom --input users --output multiservers.txt --dnsdomain westbridge.hsm \
                   --maxservers 32 --parallel 16 --server 10.0.10.15 --dump";
        let l = labels(cmd);
        for want in ["INPUT", "OUTPUT", "DOMAIN", "MAXSERVERS", "PARALLEL", "IP"] {
            assert!(l.contains(&want.to_string()), "{l:?} missing {want}");
        }
        // `--dump` takes nothing and there is nothing after it to swallow.
        assert_eq!(roundtrip(cmd), cmd);
    }

    /// Positionals are fields; the program and its subcommand are not.
    #[test]
    fn positionals_are_fields_but_subcommands_are_not() {
        let cmd = "kerbrute userenum -d westbridge.hsm --dc 10.0.10.15 ~/SecLists/Usernames/jsmith.txt";
        let l = labels(cmd);
        assert_eq!(l, vec!["DOMAIN", "DC_IP", "WORDLIST"], "{l:?}");
        assert_eq!(roundtrip(cmd), cmd);
    }

    /// `--long=VALUE` splits at the `=`; a bare `NAME=VALUE` does too.
    #[test]
    fn assignments_split_at_the_equals() {
        let (fields, slots) = detect("john --wordlist=/opt/rockyou.txt hashes.txt");
        let l: Vec<_> = fields.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(l, vec!["WORDLIST", "FILE"], "{l:?}");
        let cmd = "john --wordlist=/opt/rockyou.txt hashes.txt";
        assert_eq!(&cmd[slots[0].start..slots[0].end], "/opt/rockyou.txt");
    }

    /// A boolean flag must not eat the argument behind it.
    #[test]
    fn boolean_flags_do_not_swallow_arguments() {
        let (fields, slots) = detect("hashcat -m 1000 -a 0 --force hashes.txt rockyou.txt");
        let cmd = "hashcat -m 1000 -a 0 --force hashes.txt rockyou.txt";
        let spans: Vec<&str> = slots.iter().map(|s| &cmd[s.start..s.end]).collect();
        assert!(spans.contains(&"hashes.txt"), "{spans:?}");
        assert!(spans.contains(&"rockyou.txt"), "{spans:?}");
        let l: Vec<_> = fields.iter().map(|f| f.label.as_str()).collect();
        assert!(l.contains(&"M"), "{l:?}");
    }

    /// Only a value with a name worth reusing goes into the sticky store —
    /// `arg`/`file` mean nothing outside the one command they came from.
    #[test]
    fn positionals_do_not_pollute_the_sticky_store() {
        let (fields, _) = detect("unzip -P hunter2 backup.zip /tmp/out");
        for f in &fields {
            match f.label.as_str() {
                "FILE" | "FILE 2" | "ARG" | "WORDLIST" => {
                    assert!(!f.sticky, "{} should not stick", f.label)
                }
                _ => assert!(f.sticky, "{} should stick", f.label),
            }
        }
    }

    /// Some entries are written instructions, not command lines. Splitting
    /// those into arguments produces dozens of junk fields.
    #[test]
    fn prose_and_queries_are_left_alone() {
        for cmd in [
            "Proceed to navigate to the File menu and select Open, thereby prompting the dialog.",
            "Project > Manage NuGetPackages > Settings > Uncheck nuget.org > Apply",
            "SELECT name, type_desc FROM master.sys.server_principals WHERE type IN ('S','U');",
            "query IntrospectionQuery { __schema { queryType { name } } }",
        ] {
            let l = labels(cmd);
            assert!(
                !l.iter().any(|x| x.starts_with("ARG") || x.starts_with("FILE")),
                "{cmd:?} -> {l:?}"
            );
        }
    }

    /// An all-caps token the allowlist has never heard of still names itself,
    /// and still starts empty rather than pre-filled with its own name.
    #[test]
    fn unlisted_placeholders_name_themselves() {
        let (fields, _) = detect("kerbrute userenum 'USERS_FILE' --dc 'DC_FQDN'");
        let f = fields.iter().find(|f| f.label == "USERS_FILE").expect("USERS_FILE");
        assert_eq!(f.canon, "users_file");
        assert!(!is_literalish(&f.literal), "should not offer itself as a default");
        // A real all-caps value is not a placeholder.
        assert!(labels("certipy req -template ESC1").contains(&"TEMPLATE".to_string()));
    }

    /// The corpus invents placeholders faster than the allowlist grows; an
    /// underscore is enough to tell one from real all-caps text.
    #[test]
    fn unlisted_underscored_placeholders_are_blanks() {
        let l = labels("curl \"https://x/api\" -H \"X-API-Key: API_KEY\" -o OUT_FILE");
        assert!(l.contains(&"API_KEY".to_string()), "{l:?}");
        // Constants keep their meaning.
        for cmd in [
            "reg save HKEY_LOCAL_MACHINE\\SAM sam.save",
            "SELECT SYSTEM_USER, IS_SRVROLEMEMBER('sysadmin');",
            "sqlmap -u http://x --sql-query \"SELECT TABLE_NAME FROM INFORMATION_SCHEMA.TABLES\"",
        ] {
            let l = labels(cmd);
            for junk in ["HKEY_LOCAL_MACHINE", "SYSTEM_USER", "IS_SRVROLEMEMBER", "TABLE_NAME",
                         "INFORMATION_SCHEMA"] {
                assert!(!l.contains(&junk.to_string()), "{cmd:?} -> {l:?}");
            }
        }
        // The name carries the kind, so /etc/hosts and the tunnel IP reach it.
        let (f, _) = detect("nc EXCHANGE_FQDN 25 -s TUN_IP");
        let k = |n: &str| f.iter().find(|x| x.label == n).map(|x| x.kind);
        assert_eq!(k("EXCHANGE_FQDN"), Some(VarKind::Fqdn));
        assert_eq!(k("TUN_IP"), Some(VarKind::LocalIp));
    }

    /// `/switch` is an option, not a path — and a `-flag` in front of one is
    /// not carrying it as a value.
    #[test]
    fn windows_style_switches_are_options() {
        let cmd = "xfreerdp /u:'Guest' /v:10.0.0.5 /dynamic-resolution /bpp:8 -wallpaper /clipboard";
        let l = labels(cmd);
        assert!(l.contains(&"BPP".to_string()), "{l:?}");
        for junk in ["FILE", "WALLPAPER", "DYNAMIC_RESOLUTION"] {
            assert!(!l.contains(&junk.to_string()), "{l:?} should not contain {junk}");
        }
        assert_eq!(roundtrip(cmd), cmd);
        // A real path keeps being a path.
        assert!(labels("cat /etc/hosts").contains(&"FILE".to_string()));
    }

    /// Flag overrides key off the programs actually invoked, so a DirBuster
    /// wordlist path is not mistaken for an invocation of `dirb`.
    #[test]
    fn overrides_key_off_the_program_not_the_text() {
        let l = labels("feroxbuster -u TARGET-URL -w /opt/SecLists/DirBuster-2007-medium.txt");
        assert!(l.contains(&"URL".to_string()), "{l:?}");
        assert!(l.contains(&"WORDLIST".to_string()), "{l:?}");
        assert!(!l.contains(&"USER".to_string()), "{l:?}");
        // A program behind a pipe still counts.
        assert!(labels("cat urls | ffuf -w w.txt -u http://x/FUZZ")
            .contains(&"WORDLIST".to_string()));
    }

    /// A placeholder that runs into the next word is not a placeholder.
    #[test]
    fn partial_tokens_are_not_matched() {
        for cmd in ["feroxbuster -u TARGET-URL", "echo DOMAINNAME", "echo MY.DC_IP"] {
            assert_eq!(roundtrip(cmd), cmd);
        }
        let (_, slots) = detect("feroxbuster -u TARGET-URL");
        let cmd = "feroxbuster -u TARGET-URL";
        assert!(slots.iter().all(|s| &cmd[s.start..s.end] != "TARGET"));
    }

    /// Widening detection must not widen what gets corrupted.
    #[test]
    fn wide_detection_still_round_trips() {
        for cmd in [
            "ldapnomnom --input users --output out.txt --server 10.0.10.15 --dump",
            "kerbrute userenum -d west.hsm --dc 10.0.10.15 ~/SecLists/u.txt",
            "sudo proxychains4 nxc smb 10.0.0.0/24 -u u -p p --shares | tee out.txt",
            "docker run --rm -v $(pwd):/data alpine sh -c 'ls /data'",
            "ffuf -w /opt/w.txt:FUZZ -u http://TARGET/FUZZ -mc 200,301 -t 40",
        ] {
            assert_eq!(roundtrip(cmd), cmd, "roundtrip changed: {cmd}");
        }
    }

}
