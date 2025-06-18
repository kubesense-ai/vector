use std::borrow::Cow;
use std::{
    collections::{BTreeMap, HashMap},
    future::ready,
    num::NonZeroUsize,
};

use crate::{
    config::{
        schema::Definition, DataType, Input, LogNamespace, OutputId, TransformConfig,
        TransformContext,
    },
    event::Event,
    transforms::{TaskTransform, Transform},
};
use futures::StreamExt;
use vector_lib::config::{log_schema, TransformOutput};
use vector_lib::configurable::configurable_component;

use vector_lib::event::LogEvent;
use vrl::value::Value;

mod drain;

/// config for `kubesense_log_pattern` transform.
#[configurable_component(transform("kubesense_log_pattern"))]
#[derive(Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct KubesenseLogPatternConfig {
    /// max clusters to keep in the lru cache
    #[serde(default = "default_max_clusters")]
    pub max_clusters: usize,

    /// similarity threshold for log pattern match
    #[serde(default = "default_similarity_threshold")]
    pub similarity_threshold: f64,

    /// max node depth of the tree
    #[serde(default = "default_max_node_depth")]
    pub max_node_depth: usize,

    /// max children for a node
    #[serde(default = "default_max_children")]
    pub max_children: usize,

    /// field to which to cluster on 
    pub cluster_field: Option<String>,

    // /// field to which to cluster on 
    // pub cluster_field: Option<String>,
}

const fn default_max_clusters() -> usize {
    1000
}

const fn default_similarity_threshold() -> f64 {
    0.5
}

const fn default_max_node_depth() -> usize {
    6
}

const fn default_max_children() -> usize {
    100
}

impl_generate_config_from_default!(KubesenseLogPatternConfig);

#[async_trait::async_trait]
#[typetag::serde(name = "kubesense_log_pattern")]
impl TransformConfig for KubesenseLogPatternConfig {
    async fn build(&self, _context: &TransformContext) -> crate::Result<Transform> {
        Ok(Transform::event_task(KubesenseLogPattern::new(
            self,
        )))
    }

    fn input(&self) -> Input {
        Input::log()
    }

    fn outputs(
        &self,
        _: vector_lib::enrichment::TableRegistry,
        _: &[(OutputId, Definition)],
        _: LogNamespace,
    ) -> Vec<TransformOutput> {
        vec![TransformOutput::new(DataType::Log, HashMap::new())]
    }

    fn enable_concurrency(&self) -> bool {
        false
    }
}

struct KubesenseLogPattern {
    parser: drain::LogPatternClassifier<'static>,
    cluster_field: Option<String>,
}

impl KubesenseLogPattern {
    pub fn new(
        config: &KubesenseLogPatternConfig,
    ) -> Self {
        KubesenseLogPattern {
            parser: drain::LogPatternClassifier::new(NonZeroUsize::new(config.max_clusters).unwrap())
            .sim_threshold(config.similarity_threshold)
            .max_node_depth(config.max_node_depth)
            .max_children(config.max_children),
            cluster_field: config.cluster_field.clone(),
        }
    }

    fn transform_log(&mut self, mut event: Event) -> Option<Event> {
        let log = event.as_mut_log();
        let (Some(field_name), Some(line)) = self.get_cluster_line(log) else {
            return Some(event);
        };
        let field_name = Some(field_name.to_string());
        let (group, _group_status) = self.parser.add_log_message(line.as_ref());
        let mut cluster = BTreeMap::new();
        cluster.insert(
            "cluster_id".to_string().into(),
            Value::Bytes(group.cluster_id().into()),
        );
        cluster.insert(
            "match_count".to_string().into(),
            Value::Integer(group.cluster_size() as i64),
        );
        cluster.insert(
            "template".to_string().into(),
            Value::Bytes(format!("{}", group).into()),
        );
        log.insert(
            field_name.expect("set field name!!").as_str() ,
            Value::Object(cluster),
        );
        Some(event)
    }

    fn get_cluster_line<'a>(
        &self,
        log: &'a LogEvent,
    ) -> (Option<Cow<'a, str>>, Option<Cow<'a, str>>) {
        let field_name: Option<Cow<'a, str>> = if let Some(field_name) = self.cluster_field.as_ref()
        {
            Some(Cow::Owned(field_name.as_str().to_string()))
        } else if let Some(field_name) =
            log.get((log_schema().message_key().unwrap().to_string() + ".message_key").as_str())
        {
            field_name.as_str()
        } else {
            None
        };

        if field_name.is_none() {
            let field_name = log_schema().message_key().unwrap().to_string();
            if let Some(field) = log.get(log_schema().message_key_target_path().unwrap()) {
                if field.is_bytes() {
                    return (Some(Cow::Owned(field_name)), field.as_str());
                }
            }
            return (None, None);
        }

        let line = field_name
            .as_ref()
            .and_then(|name| log.get(name.as_ref()))
            .and_then(|f| f.as_str());

        (field_name, line)
    }
}

impl TaskTransform<Event> for KubesenseLogPattern {
    fn transform(
        self: Box<Self>,
        task: std::pin::Pin<Box<dyn futures_util::Stream<Item = Event> + Send>>,
    ) -> std::pin::Pin<Box<dyn futures_util::Stream<Item = Event> + Send>> {
        let mut inner = self;
        Box::pin(task.filter_map(move |v| ready(inner.transform_log(v))))
    }
}
