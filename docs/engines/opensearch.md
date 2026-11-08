# OpenSearch

Engine id `opensearch`. It shares the Elasticsearch adapter and UI module; see [elasticsearch.md](elasticsearch.md) for the tree,
the document grid, keys and edits, the console, the gateway rules and the error mapping.

What differs:

- **Server check**: the cluster must report the `opensearch` distribution (an Elasticsearch server is refused with a message naming
  the Elasticsearch engine). OpenSearch 1.0 or newer; point-in-time paging past 10,000 hits needs 2.4.
- **Auth**: basic (user and password), bearer token or none. With the security plugin enabled the cluster needs basic auth and
  https; the demo configuration's user is `admin`, and a 401 carries that hint. No API keys and no Cloud ID.
- **Languages**: the console and SQL (`_plugins/_sql`, JDBC format). No ES|QL.
- **APIs**: point in time is `_search/point_in_time` (`pit_id`), lifecycle policies are ISM (`_plugins/_ism/policies`), and `_id`
  can be sorted (Elasticsearch 8 cannot); a declared key (`_meta.fosta.key`) is still needed to range it.

Dev server: `dev/compose/opensearch.yml` (OpenSearch 2.19 on http://127.0.0.1:9201, security plugin off). Contract suites:
`cargo test contract_opensearch -- --ignored --test-threads=1`.
