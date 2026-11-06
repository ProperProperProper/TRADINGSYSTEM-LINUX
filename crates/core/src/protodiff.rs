//! Field-by-field comparison of two sets of .proto files (moomoo SDK vs the bot), so a protocol upgrade can be
//! classified as additive or breaking from the files themselves.
use anyhow::Result;
use std::{collections::BTreeMap, fs, path::Path};

/// key (`Message.field` / `Enum.VALUE`) -> normalised declaration (`repeated string=3`).
pub type Decls = BTreeMap<String, String>;

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if b[i] == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

fn tokens(s: &str) -> Vec<String> {
    let mut t = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    for c in s.chars() {
        if in_str {
            cur.push(c);
            if c == '"' {
                in_str = false;
                t.push(std::mem::take(&mut cur));
            }
        } else if c == '"' {
            if !cur.is_empty() {
                t.push(std::mem::take(&mut cur));
            }
            cur.push(c);
            in_str = true;
        } else if c.is_whitespace() {
            if !cur.is_empty() {
                t.push(std::mem::take(&mut cur));
            }
        } else if "{};=<>,()[]".contains(c) {
            if !cur.is_empty() {
                t.push(std::mem::take(&mut cur));
            }
            t.push(c.to_string());
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        t.push(cur);
    }
    t
}

/// Declarations of one .proto source.
pub fn parse(src: &str) -> Decls {
    let toks = tokens(&strip_comments(src));
    let mut out = Decls::new();
    // scope: (name, is_enum, is_transparent) where transparent scopes (oneof) do not extend the path
    let mut scope: Vec<(String, bool, bool)> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let mut j = i;
        while j < toks.len() && !matches!(toks[j].as_str(), ";" | "{" | "}") {
            j += 1;
        }
        let stmt = &toks[i..j];
        match toks.get(j).map(String::as_str) {
            Some("{") => {
                let kind = stmt.first().map(String::as_str).unwrap_or("");
                let name = stmt.get(1).cloned().unwrap_or_default();
                match kind {
                    "message" | "enum" | "service" | "extend" => {
                        scope.push((name, kind == "enum", false))
                    }
                    "oneof" => scope.push((name, false, true)),
                    _ => scope.push((String::new(), false, true)), // option { ... } blocks
                }
            }
            Some("}") => {
                scope.pop();
            }
            _ => {
                if let Some(eq) = stmt.iter().position(|t| t == "=") {
                    let path = scope
                        .iter()
                        .filter(|s| !s.2)
                        .map(|s| s.0.as_str())
                        .collect::<Vec<_>>()
                        .join(".");
                    let in_enum = scope.iter().rev().find(|s| !s.2).is_some_and(|s| s.1);
                    let first = stmt.first().map(String::as_str).unwrap_or("");
                    if !matches!(
                        first,
                        "syntax" | "package" | "import" | "option" | "reserved" | "extensions"
                    ) && eq >= 1
                    {
                        let num = stmt.get(eq + 1).cloned().unwrap_or_default();
                        if in_enum {
                            out.insert(format!("{path}.{}", stmt[0]), format!("={num}"));
                        } else if eq >= 2 {
                            let ty = stmt[..eq - 1].join(" ");
                            out.insert(format!("{path}.{}", stmt[eq - 1]), format!("{ty}={num}"));
                        }
                    }
                }
            }
        }
        i = j + 1;
    }
    out
}

#[derive(Debug, Default, PartialEq)]
pub struct FileDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<(String, String, String)>,
    /// Same message, same field number and type, new name: wire-compatible, but generated Rust field names change.
    pub renamed: Vec<(String, String)>,
}

/// (type without the label, number) from a declaration like `optional string=2`.
fn split_decl(decl: &str) -> (String, String) {
    let (ty, num) = decl.rsplit_once('=').unwrap_or((decl, ""));
    let ty = ty.trim();
    let ty = ["required ", "optional ", "repeated "]
        .iter()
        .find_map(|l| ty.strip_prefix(l))
        .unwrap_or(ty);
    (ty.to_string(), num.to_string())
}

impl FileDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
            && self.renamed.is_empty()
    }
    /// Breaking for an old client reading new data: a removed or retyped/renumbered field.
    pub fn breaking(&self) -> bool {
        !self.removed.is_empty() || !self.changed.is_empty()
    }
}

/// Difference going from `old` (the bot's file) to `new` (the SDK's file).
pub fn diff(old: &Decls, new: &Decls) -> FileDiff {
    let mut d = FileDiff::default();
    for (k, v) in new {
        match old.get(k) {
            None => d.added.push(format!("{k} {v}")),
            Some(o) if o != v => d.changed.push((k.clone(), o.clone(), v.clone())),
            _ => {}
        }
    }
    let removed: Vec<&String> = old.keys().filter(|k| !new.contains_key(*k)).collect();
    let mut gone = Vec::new();
    for k in removed {
        let (path, _) = k.rsplit_once('.').unwrap_or(("", k));
        let (ty, num) = split_decl(&old[k]);
        // an added field in the same message with the same number and type is a rename
        let hit = d.added.iter().position(|a| {
            let (ak, ad) = a.split_once(' ').unwrap_or((a, ""));
            let (apath, _) = ak.rsplit_once('.').unwrap_or(("", ak));
            let (aty, anum) = split_decl(ad);
            apath == path && anum == num && aty == ty
        });
        match hit {
            Some(i) => {
                let a = d.added.remove(i);
                d.renamed
                    .push((k.clone(), a.split(' ').next().unwrap_or("").to_string()));
            }
            None => gone.push(format!("{k} {}", old[k])),
        }
    }
    d.removed = gone;
    d
}

pub struct Report {
    pub shared: usize,
    pub identical: usize,
    pub additive: Vec<(String, FileDiff)>,
    pub breaking: Vec<(String, FileDiff)>,
    pub only_new: Vec<String>,
    pub only_old: Vec<String>,
}

fn load_dir(dir: &Path) -> Result<BTreeMap<String, Decls>> {
    let mut m = BTreeMap::new();
    for e in fs::read_dir(dir)? {
        let p = e?.path();
        if p.extension().is_some_and(|x| x == "proto") {
            m.insert(
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                parse(&fs::read_to_string(&p)?),
            );
        }
    }
    Ok(m)
}

/// Compares the bot's proto directory (`old`) with the SDK's (`new`).
pub fn compare_dirs(old: &Path, new: &Path) -> Result<Report> {
    let (o, n) = (load_dir(old)?, load_dir(new)?);
    let mut r = Report {
        shared: 0,
        identical: 0,
        additive: vec![],
        breaking: vec![],
        only_new: vec![],
        only_old: vec![],
    };
    for (name, nd) in &n {
        match o.get(name) {
            None => r.only_new.push(name.clone()),
            Some(od) => {
                r.shared += 1;
                let d = diff(od, nd);
                if d.is_empty() {
                    r.identical += 1
                } else if d.breaking() {
                    r.breaking.push((name.clone(), d))
                } else {
                    r.additive.push((name.clone(), d))
                }
            }
        }
    }
    r.only_old = o.keys().filter(|k| !n.contains_key(*k)).cloned().collect();
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = r#"syntax = "proto2"; package A; // c
        message M { required int32 a = 1; optional string b = 2; /* x */ repeated Foo c = 3; oneof o { int32 d = 4; } map<string, int32> e = 5; }
        enum E { X = 0; Y = 1; }"#;

    #[test]
    fn parses_fields_enums_oneof_and_maps() {
        let d = parse(OLD);
        assert_eq!(d["M.a"], "required int32=1");
        assert_eq!(d["M.c"], "repeated Foo=3");
        assert_eq!(d["M.d"], "int32=4", "oneof members belong to the message");
        assert_eq!(d["M.e"], "map < string , int32 >=5");
        assert_eq!(d["E.Y"], "=1");
        assert!(
            !d.keys()
                .any(|k| k.contains("syntax") || k.contains("package"))
        );
    }

    #[test]
    fn a_rename_with_the_same_number_and_type_is_not_breaking() {
        let old =
            parse("message M { optional int32 used_sub_quota = 1; optional int32 keep = 2; }");
        let new = parse("message M { optional int32 usedSubQuota = 1; optional int32 keep = 2; }");
        let d = diff(&old, &new);
        assert_eq!(
            d.renamed,
            vec![("M.used_sub_quota".to_string(), "M.usedSubQuota".to_string())]
        );
        assert!(!d.breaking() && d.added.is_empty() && d.removed.is_empty());
        let retyped =
            parse("message M { optional int64 usedSubQuota = 1; optional int32 keep = 2; }");
        assert!(
            diff(&old, &retyped).breaking(),
            "same number but a new type is breaking"
        );
    }

    #[test]
    fn classifies_additive_and_breaking() {
        let old = parse(OLD);
        let added = parse(&OLD.replace(
            "repeated Foo c = 3;",
            "repeated Foo c = 3; optional bool z = 9;",
        ));
        let d = diff(&old, &added);
        assert_eq!(d.added, vec!["M.z optional bool=9".to_string()]);
        assert!(!d.breaking());
        let retyped = parse(&OLD.replace("optional string b = 2;", "optional bytes b = 2;"));
        assert!(diff(&old, &retyped).breaking());
        let removed = parse(&OLD.replace("optional string b = 2;", ""));
        assert!(diff(&old, &removed).breaking());
        assert!(diff(&old, &old).is_empty());
    }
}
