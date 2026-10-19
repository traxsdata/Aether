//! matching.rs - does a selector match an element?
//!
//! Right to left, with backtracking for descendant and sibling combinators.
//! Every match gets a step budget, so selectors built to explode (`a a a a
//! ... b` against a deep tree) cost a bounded amount and simply fail.

use super::parser::{AttrOp, AttrSel, Combinator, Compound, Pseudo, Selector};
use crate::dom::{Document, NodeData, NodeId};

pub const BUDGET: u32 = 4000;

pub fn matches(doc: &Document, el: NodeId, sel: &Selector) -> bool {
    let mut budget = BUDGET;
    match_from(doc, el, sel, sel.compounds.len() - 1, &mut budget)
}

fn parent_element(doc: &Document, n: NodeId) -> Option<NodeId> {
    let p = doc.parent(n)?;
    doc.element(p).map(|_| p)
}

fn prev_element(doc: &Document, n: NodeId) -> Option<NodeId> {
    let p = doc.parent(n)?;
    let sibs = doc.children(p);
    let i = sibs.iter().position(|&c| c == n)?;
    sibs[..i].iter().rev().copied().find(|&c| doc.element(c).is_some())
}

fn match_from(doc: &Document, el: NodeId, sel: &Selector, idx: usize, budget: &mut u32) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    if !compound_matches(doc, el, &sel.compounds[idx], budget) {
        return false;
    }
    if idx == 0 {
        return true;
    }
    match sel.combinators[idx - 1] {
        Combinator::Child => parent_element(doc, el).is_some_and(|p| match_from(doc, p, sel, idx - 1, budget)),
        Combinator::Descendant => {
            let mut a = parent_element(doc, el);
            while let Some(p) = a {
                if match_from(doc, p, sel, idx - 1, budget) {
                    return true;
                }
                if *budget == 0 {
                    return false;
                }
                a = parent_element(doc, p);
            }
            false
        }
        Combinator::Next => prev_element(doc, el).is_some_and(|p| match_from(doc, p, sel, idx - 1, budget)),
        Combinator::Subsequent => {
            let mut s = prev_element(doc, el);
            while let Some(p) = s {
                if match_from(doc, p, sel, idx - 1, budget) {
                    return true;
                }
                if *budget == 0 {
                    return false;
                }
                s = prev_element(doc, p);
            }
            false
        }
    }
}

fn attr_matches(value: Option<&str>, a: &AttrSel) -> bool {
    let Some(v) = value else { return false };
    let norm = |s: &str| if a.ci { s.to_ascii_lowercase() } else { s.to_string() };
    let v = norm(v);
    match &a.op {
        AttrOp::Exists => true,
        AttrOp::Eq(x) => v == norm(x),
        AttrOp::Word(x) => !x.is_empty() && v.split_ascii_whitespace().any(|w| w == norm(x)),
        AttrOp::Dash(x) => v == norm(x) || v.starts_with(&format!("{}-", norm(x))),
        AttrOp::Prefix(x) => !x.is_empty() && v.starts_with(&norm(x)),
        AttrOp::Suffix(x) => !x.is_empty() && v.ends_with(&norm(x)),
        AttrOp::Contains(x) => !x.is_empty() && v.contains(&norm(x)),
    }
}

/// Position among element siblings: (index from 1, count), optionally only
/// counting siblings with the same name.
fn position(doc: &Document, el: NodeId, same_type: bool) -> (i32, i32) {
    let Some(p) = doc.parent(el) else { return (1, 1) };
    let name = doc.name(el);
    let mut idx = 0;
    let mut count = 0;
    for &c in doc.children(p) {
        if let Some(e) = doc.element(c) {
            if same_type && e.name != name {
                continue;
            }
            count += 1;
            if c == el {
                idx = count;
            }
        }
    }
    (idx, count)
}

fn nth(a: i32, b: i32, pos: i32) -> bool {
    if a == 0 {
        return pos == b;
    }
    let d = pos - b;
    d % a == 0 && d / a >= 0
}

fn compound_matches(doc: &Document, el: NodeId, c: &Compound, budget: &mut u32) -> bool {
    let Some(e) = doc.element(el) else { return false };
    if let Some(t) = &c.tag {
        if !e.name.eq_ignore_ascii_case(t) {
            return false;
        }
    }
    if let Some(id) = &c.id {
        if e.id() != Some(id.as_str()) {
            return false;
        }
    }
    for cl in &c.classes {
        if !e.has_class(cl) {
            return false;
        }
    }
    for a in &c.attrs {
        if !attr_matches(e.attr(&a.name), a) {
            return false;
        }
    }
    for p in &c.pseudos {
        let ok = match p {
            Pseudo::Link => matches!(e.name.as_str(), "a" | "area") && e.attr("href").is_some(),
            Pseudo::Never | Pseudo::Has => false,
            Pseudo::Root => doc.parent(el) == Some(Document::ROOT),
            Pseudo::Empty => doc.children(el).iter().all(|&ch| match &doc.nodes[ch].data {
                NodeData::Element(_) => false,
                NodeData::Text(t) => t.is_empty(),
                _ => true,
            }),
            Pseudo::FirstChild => position(doc, el, false).0 == 1,
            Pseudo::LastChild => {
                let (i, n) = position(doc, el, false);
                i == n
            }
            Pseudo::OnlyChild => position(doc, el, false).1 == 1,
            Pseudo::FirstOfType => position(doc, el, true).0 == 1,
            Pseudo::LastOfType => {
                let (i, n) = position(doc, el, true);
                i == n
            }
            Pseudo::OnlyOfType => position(doc, el, true).1 == 1,
            Pseudo::NthChild(a, b, last) | Pseudo::NthOfType(a, b, last) => {
                let (i, n) = position(doc, el, matches!(p, Pseudo::NthOfType(..)));
                nth(*a, *b, if *last { n - i + 1 } else { i })
            }
            Pseudo::Checked => e.attr("checked").is_some() || e.attr("selected").is_some(),
            Pseudo::Disabled => e.attr("disabled").is_some(),
            Pseudo::Enabled => matches!(e.name.as_str(), "input" | "button" | "select" | "textarea") && e.attr("disabled").is_none(),
            Pseudo::Not(list) => !list.iter().any(|s| match_from(doc, el, s, s.compounds.len() - 1, budget)),
            Pseudo::Is(list) | Pseudo::Where(list) => list.iter().any(|s| s.pseudo_element.is_none() && match_from(doc, el, s, s.compounds.len() - 1, budget)),
        };
        if !ok {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::super::parser::parse_selector;
    use super::super::tokenizer::{component_values, tokenize};
    use super::*;
    use crate::html::parse;

    fn sel(s: &str) -> Selector {
        parse_selector(&component_values(tokenize(s))).unwrap()
    }

    #[test]
    fn basic_matching() {
        let d = parse("<div id=main class='a b'><ul><li>1</li><li class=x>2</li><li>3</li></ul><p><a href=/>l</a><a>n</a></p></div>");
        let lis = d.find_all("li");
        let a = d.find_all("a");
        assert!(matches(&d, lis[0], &sel("#main li")));
        assert!(matches(&d, lis[0], &sel("div.a.b > ul > li:first-child")));
        assert!(!matches(&d, lis[0], &sel("div > li")));
        assert!(matches(&d, lis[1], &sel("li + .x")));
        assert!(!matches(&d, lis[0], &sel("li + li")));
        assert!(matches(&d, lis[1], &sel(".x")));
        assert!(matches(&d, lis[2], &sel(".x + li")));
        assert!(matches(&d, lis[2], &sel("li ~ li:last-child")));
        assert!(matches(&d, lis[1], &sel("li:nth-child(2n)")));
        assert!(!matches(&d, lis[2], &sel("li:nth-child(2n)")));
        assert!(matches(&d, lis[2], &sel("li:nth-last-child(1)")));
        assert!(matches(&d, a[0], &sel("a:link")));
        assert!(!matches(&d, a[1], &sel("a:link")));
        assert!(!matches(&d, a[0], &sel("a:visited")));
        assert!(matches(&d, a[0], &sel("a[href='/']")));
        assert!(matches(&d, a[1], &sel("a:not([href])")));
        assert!(matches(&d, a[1], &sel(":is(p, span) > a:last-child")));
        let html = d.html_element().unwrap();
        assert!(matches(&d, html, &sel(":root")));
    }

    #[test]
    fn explosive_selector_is_bounded() {
        let html = "<div>".repeat(150) + "<span>x</span>";
        let d = parse(&html);
        let span = d.find("span").unwrap();
        let s = sel(&("div ".repeat(20) + "p span"));
        let t = std::time::Instant::now();
        assert!(!matches(&d, span, &s));
        assert!(t.elapsed().as_millis() < 500);
        let s = sel(&("div ".repeat(20) + "span"));
        assert!(matches(&d, span, &s));
    }
}
