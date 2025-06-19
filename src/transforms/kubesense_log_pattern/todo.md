1. add regex preprocessing for eliminating known patterns, for masking data and for making further clustering easier.
2. add parameter extraction for getting insights on pattern data.
3. add restart resilience by storing the clustering data in an external store(redis, mysql...).
4. fine tune the config by changing the simth, max_cluster, max_children, max_node_depth.
5. add training mode, where we can train on lower env and then deploy the trained model in prod env.