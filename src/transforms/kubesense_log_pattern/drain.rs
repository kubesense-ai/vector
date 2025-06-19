use base64::Engine;
use std::{borrow::Cow, collections::HashMap, fmt::Display, hash::Hasher, num::NonZeroUsize};
use xxhash_rust::xxh3::Xxh3;

pub type LocalId = usize;

#[derive(Debug)]
pub struct LogCluster<'a> {
    template_tokens: Vec<Token<'a>>,
    cluster_size: usize,
    cluster_local_id: LocalId,
}

impl<'a> LogCluster<'a> {
    pub const fn cluster_size(&self) -> usize {
        self.cluster_size
    }

    pub fn cluster_id(&self) -> String {
        let mut hasher = Xxh3::new();
        for token in &self.template_tokens {
            token.hash(&mut hasher);
        }
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(hasher.finish().to_be_bytes())
    }

    fn new(cluster_id: usize, parameterize_numeric_tokens: bool, tokens: Vec<&str>) -> Self {
        Self {
            template_tokens: tokens
                .iter()
                .map(|token| {
                    if parameterize_numeric_tokens && token.chars().any(char::is_numeric) {
                        Token::Wildcard
                    } else {
                        Token::Value(Cow::Owned(token.to_string()))
                    }
                })
                .collect(),
            cluster_size: 1,
            cluster_local_id: cluster_id,
        }
    }

    fn seq_dist(&self, tokens: &Tokens) -> (f64, usize) {
        assert!(self.template_tokens.len() == tokens.len());

        if tokens.is_empty() {
            return (1.0, 0);
        }

        let mut sim_count = 0;
        let mut param_count = 0;

        for (token1, token2) in self.template_tokens.iter().zip(tokens.iter()) {
            match token1 {
                Token::Wildcard => {
                    param_count += 1;
                }
                Token::Value(token1) => {
                    if token1 == token2 {
                        sim_count += 1;
                    }
                }
            }
        }

        (
            sim_count as f64 / self.template_tokens.len() as f64,
            param_count,
        )
    }

    fn update(&mut self, tokens: &Tokens) -> bool {
        assert_eq!(self.template_tokens.len(), tokens.len());
        let mut updated = false;
        for (template_token1, token2) in self.template_tokens.iter_mut().zip(tokens.iter()) {
            match template_token1 {
                Token::Wildcard => {}
                Token::Value(token1) => {
                    if token1 != token2 {
                        *template_token1 = Token::Wildcard;
                        updated = true;
                    }
                }
            }
        }
        updated
    }
}

impl<'a> Display for LogCluster<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for token in &self.template_tokens {
            if !first {
                write!(f, " ")?;
            }
            write!(f, "{ }", token)?;
            first = false;
        }
        Ok(())
    }
}

#[derive(Hash, PartialEq, Eq, Clone, Debug)]
enum Token<'a> {
    Wildcard,
    Value(Cow<'a, str>),
}

impl<'a> Token<'a> {
    fn has_numbers(&self) -> bool {
        match self {
            Token::Wildcard => false,
            Token::Value(s) => s.chars().any(char::is_numeric),
        }
    }

    fn hash(&self, hasher: &mut Xxh3) {
        match self {
            Token::Wildcard => {
                hasher.update(&1u8.to_le_bytes());
            }
            Token::Value(s) => {
                hasher.update(&2u8.to_le_bytes());
                hasher.update(s.as_bytes());
            }
        }
    }
}

impl<'a> Display for Token<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Token::Wildcard => write!(f, "<*>"),
            Token::Value(value) => write!(f, "{}", value),
        }
    }
}

#[derive(Debug)]
struct Node<'a> {
    children: HashMap<Token<'a>, Node<'a>>,
    cluster_ids: Vec<usize>,
}

impl<'a> Node<'a> {
    fn new() -> Self {
        Self {
            children: HashMap::new(),
            cluster_ids: Vec::new(),
        }
    }
}

type Tokens<'a> = Vec<&'a str>;

#[derive(PartialEq)]
pub enum LogClusterStatus {
    ChangedTemplate,
    None,
}

pub struct LogPatternClassifier<'a> {
    root_node: HashMap<usize, Node<'a>>,
    id_to_clusters: lru::LruCache<usize, LogCluster<'a>>,
    clusters_count: usize,
    sim_threshold: f64,
    max_node_depth: usize,
    max_children: usize,
    parameterize_numeric_tokens: bool,
    extra_delimiters: Vec<char>,
}

impl<'a> LogPatternClassifier<'a> {
    pub fn new(max_clusters: NonZeroUsize) -> Self {
        Self {
            root_node: HashMap::new(),
            id_to_clusters: lru::LruCache::new(max_clusters),
            clusters_count: 0,
            sim_threshold: 0.4,
            max_node_depth: 8,
            max_children: 100,
            parameterize_numeric_tokens: true,
            extra_delimiters: Vec::new(),
        }
    }

    pub const fn sim_threshold(mut self, value: f64) -> Self {
        self.sim_threshold = value;
        self
    }

    pub const fn max_node_depth(mut self, value: usize) -> Self {
        self.max_node_depth = value;
        self
    }

    pub const fn max_children(mut self, value: usize) -> Self {
        self.max_children = value;
        self
    }

    #[allow(dead_code)]
    pub const fn parameterize_numeric_tokens(mut self, value: bool) -> Self {
        self.parameterize_numeric_tokens = value;
        self
    }

    #[allow(dead_code)]
    pub fn extra_delimiters(mut self, value: Vec<char>) -> Self {
        self.extra_delimiters = value;
        self
    }

    pub fn add_log_message(&mut self, line: &str) -> (&LogCluster, LogClusterStatus) {
        let tokens = tokenize(line, &self.extra_delimiters);

        match self.tree_search(&tokens) {
            None => {
                self.clusters_count += 1;
                let cluster_id = self.clusters_count;
                let cluster = LogCluster::new(cluster_id, self.parameterize_numeric_tokens, tokens);
                self.add_seq_to_prefix_tree(&cluster);
                (
                    self.id_to_clusters.get_or_insert(cluster_id, || cluster),
                    LogClusterStatus::ChangedTemplate,
                )
            }
            Some(cluster_id) => {
                let cluster = self.id_to_clusters.get_mut(&cluster_id).unwrap();
                cluster.cluster_size += 1;
                let updated = cluster.update(&tokens);
                (
                    cluster,
                    if updated {
                        LogClusterStatus::ChangedTemplate
                    } else {
                        LogClusterStatus::None
                    },
                )
            }
        }
    }

    fn add_seq_to_prefix_tree(&mut self, cluster: &LogCluster<'a>) {
        let token_count = cluster.template_tokens.len();
        let mut curr_node = self
            .root_node
            .entry(token_count)
            .or_insert_with(Node::new);

        if token_count == 0 {
            curr_node.cluster_ids.push(cluster.cluster_local_id);
            return;
        }

        let mut curr_node_depth = 1;
        for token in &cluster.template_tokens {
            let max_children = if curr_node_depth > 1 {
                let factor = curr_node_depth * 2;
                let result = self.max_children / factor;
                if result < 2 {
                    2
                } else {
                    result
                }
            } else {
                self.max_children
            };
            if curr_node_depth >= self.max_node_depth || curr_node_depth >= token_count {
                break;
            }

            curr_node = if curr_node.children.contains_key(token) {
                curr_node.children.get_mut(token).unwrap()
            } else if self.parameterize_numeric_tokens && token.has_numbers() {
                curr_node
                    .children
                    .entry(Token::Wildcard)
                    .or_insert_with(Node::new)
            } else if curr_node.children.contains_key(&Token::Wildcard) {
                if curr_node.children.len() < max_children {
                    curr_node
                        .children
                        .entry(token.clone())
                        .or_insert_with(Node::new)
                } else {
                    curr_node.children.get_mut(&Token::Wildcard).unwrap()
                }
            } else if curr_node.children.len() + 1 < max_children {
                curr_node
                    .children
                    .entry(token.clone())
                    .or_insert_with(Node::new)
            } else if curr_node.children.len() + 1 == max_children {
                curr_node
                    .children
                    .entry(Token::Wildcard)
                    .or_insert_with(Node::new)
            } else {
                unreachable!();
            };

            curr_node_depth += 1;
        }

        let cluster_id = cluster.cluster_local_id;
        let mut new_cluster_ids = Vec::new();
        for cluster_id in &curr_node.cluster_ids {
            if self.id_to_clusters.contains(cluster_id) {
                new_cluster_ids.push(*cluster_id);
            }
        }
        new_cluster_ids.push(cluster_id);
        curr_node.cluster_ids = new_cluster_ids;
    }

    fn tree_search(&mut self, tokens: &Tokens) -> Option<usize> {
        let token_count = tokens.len();

        let mut curr_node = self.root_node.get(&token_count);
        let mut curr_node_depth = 1;

        for token in tokens {
            if curr_node_depth >= self.max_node_depth {
                break;
            }
            if curr_node_depth == token_count {
                break;
            }

            match curr_node {
                None => break,
                Some(node) => {
                    curr_node = node.children.get(&Token::Value(Cow::Borrowed(token)));
                    if curr_node.is_none() {
                        curr_node = node.children.get(&Token::Wildcard);
                    }
                }
            }

            curr_node_depth += 1
        }

        match curr_node {
            None => None,
            Some(node) => {
                let mut max_sim = 0.0;
                let mut max_param_count = 0;
                let mut max_cluster_id: Option<usize> = None;

                for cluster_id in &node.cluster_ids {
                    let cluster = self.id_to_clusters.get(cluster_id);
                    match cluster {
                        None => continue,
                        Some(cluster) => {
                            let (sim, param_count) = cluster.seq_dist(tokens);
                            if sim > max_sim || (sim == max_sim && param_count > max_param_count) {
                                max_sim = sim;
                                max_param_count = param_count;
                                max_cluster_id = Some(*cluster_id);
                            }
                        }
                    }
                }

                if max_sim >= self.sim_threshold {
                    max_cluster_id
                } else {
                    None
                }
            }
        }
    }
}

fn tokenize<'a>(s: &'a str, extra_delimiters: &[char]) -> Tokens<'a> {
    s.split(|c: char| c.is_whitespace() || extra_delimiters.contains(&c))
        .filter(|s| !s.is_empty())
        .collect::<Tokens<'a>>()
}
