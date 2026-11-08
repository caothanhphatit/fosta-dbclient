/**
 * OpenSearch UI module: the same views and languages as Elasticsearch (the backend descriptor differs only in what the
 * server offers), so it re-exports that module under its own id. The OpenSearch agent overrides only what differs.
 */
import type { EngineUiModule } from "../index";
import elasticsearch from "../elasticsearch/index";

const opensearch: EngineUiModule = { ...elasticsearch, id: "opensearch" };

export default opensearch;
