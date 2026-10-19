//! dom.rs - the document tree: an arena of nodes addressed by index.
//!
//! Node 0 is the Document. Elements keep their lower-case local name and
//! their attributes in source order (duplicates dropped, as HTML says).

pub type NodeId = usize;

#[derive(Clone, Debug, PartialEq)]
pub enum NodeData {
    Document,
    Doctype(String),
    Element(Element),
    Text(String),
    Comment(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    /// svg / math content: shown as nothing, its text never laid out.
    pub foreign: bool,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
    pub fn id(&self) -> Option<&str> {
        self.attr("id")
    }
    pub fn has_class(&self, class: &str) -> bool {
        self.attr("class").is_some_and(|c| c.split_ascii_whitespace().any(|x| x == class))
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub data: NodeData,
}

#[derive(Clone, Debug)]
pub struct Document {
    pub nodes: Vec<Node>,
    /// The document is in quirks mode (no or an old doctype).
    pub quirks: bool,
}

impl Default for Document {
    fn default() -> Self {
        Document { nodes: vec![Node { parent: None, children: Vec::new(), data: NodeData::Document }], quirks: false }
    }
}

impl Document {
    pub const ROOT: NodeId = 0;

    pub fn new_node(&mut self, data: NodeData) -> NodeId {
        self.nodes.push(Node { parent: None, children: Vec::new(), data });
        self.nodes.len() - 1
    }

    pub fn append(&mut self, parent: NodeId, child: NodeId) {
        self.detach(child);
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
    }

    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, before: NodeId) {
        self.detach(child);
        self.nodes[child].parent = Some(parent);
        let pos = self.nodes[parent].children.iter().position(|&c| c == before).unwrap_or(self.nodes[parent].children.len());
        self.nodes[parent].children.insert(pos, child);
    }

    pub fn detach(&mut self, child: NodeId) {
        if let Some(p) = self.nodes[child].parent.take() {
            self.nodes[p].children.retain(|&c| c != child);
        }
    }

    pub fn element(&self, id: NodeId) -> Option<&Element> {
        match &self.nodes.get(id)?.data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut Element> {
        match &mut self.nodes.get_mut(id)?.data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    pub fn name(&self, id: NodeId) -> &str {
        self.element(id).map_or("", |e| e.name.as_str())
    }

    pub fn is(&self, id: NodeId, name: &str) -> bool {
        self.element(id).is_some_and(|e| e.name == name && !e.foreign)
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id)?.parent
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id].children
    }

    /// Element children only.
    pub fn child_elements(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes[id].children.iter().copied().filter(|&c| self.element(c).is_some())
    }

    /// All nodes in tree order (iterative, so depth is no problem).
    pub fn descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            out.push(n);
            for &c in self.nodes[n].children.iter().rev() {
                stack.push(c);
            }
        }
        out
    }

    pub fn find(&self, name: &str) -> Option<NodeId> {
        self.descendants(Self::ROOT).into_iter().find(|&n| self.is(n, name))
    }

    pub fn find_all(&self, name: &str) -> Vec<NodeId> {
        self.descendants(Self::ROOT).into_iter().filter(|&n| self.is(n, name)).collect()
    }

    /// The text of every Text descendant, concatenated.
    pub fn text_content(&self, id: NodeId) -> String {
        let mut s = String::new();
        for n in self.descendants(id) {
            if let NodeData::Text(t) = &self.nodes[n].data {
                s.push_str(t);
            }
        }
        s
    }

    pub fn html_element(&self) -> Option<NodeId> {
        self.child_elements(Self::ROOT).find(|&c| self.is(c, "html"))
    }

    pub fn body(&self) -> Option<NodeId> {
        let html = self.html_element()?;
        self.child_elements(html).find(|&c| self.is(c, "body"))
    }

    /// The page's <title>, with white space collapsed.
    pub fn title(&self) -> String {
        let t = self.find("title").map(|t| self.text_content(t)).unwrap_or_default();
        let t: String = t.chars().take(1024).collect();
        t.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Depth of the deepest node (for tests).
    pub fn depth(&self) -> usize {
        let mut max = 0;
        let mut stack = vec![(Self::ROOT, 0usize)];
        while let Some((n, d)) = stack.pop() {
            max = max.max(d);
            for &c in &self.nodes[n].children {
                stack.push((c, d + 1));
            }
        }
        max
    }

    /// A compact outline of the tree, for tests: `<p>[text]</p>` style.
    pub fn outline(&self, id: NodeId) -> String {
        let mut s = String::new();
        self.outline_into(id, &mut s, 0);
        s
    }

    fn outline_into(&self, id: NodeId, s: &mut String, depth: usize) {
        if depth > 600 {
            return;
        }
        match &self.nodes[id].data {
            NodeData::Document => {
                for &c in &self.nodes[id].children {
                    self.outline_into(c, s, depth + 1);
                }
            }
            NodeData::Doctype(n) => s.push_str(&format!("<!{n}>")),
            NodeData::Comment(_) => {}
            NodeData::Text(t) => s.push_str(&format!("\"{t}\"")),
            NodeData::Element(e) => {
                s.push('<');
                s.push_str(&e.name);
                for (k, v) in &e.attrs {
                    s.push_str(&format!(" {k}={v:?}"));
                }
                s.push('>');
                for &c in &self.nodes[id].children {
                    self.outline_into(c, s, depth + 1);
                }
                s.push_str("</");
                s.push_str(&e.name);
                s.push('>');
            }
        }
    }
}
