/**
 * OpenSearch 2.19 mock: the Elasticsearch mock's data and console (`elasticsearch.ts`) under the OpenSearch
 * descriptor, without the vector field and without ES|QL.
 */
import type { EngineDescriptor } from "../../../core/contract";
import { DESCRIPTORS, ORDER } from "../descriptors";
import type { EngineMock } from "../types";
import { classify as classifyConsole, createSearchMock } from "./elasticsearch";

const mock = createSearchMock({ id: "opensearch", label: "OpenSearch", version: "2.19.6" }, DESCRIPTORS.opensearch);

export const descriptor: EngineDescriptor = mock.descriptor;
export const order = ORDER.indexOf("opensearch");
export const connectInfo = mock.connectInfo;
export const tree: NonNullable<EngineMock["tree"]> = mock.tree;
export const describe: NonNullable<EngineMock["describe"]> = mock.describe;
export const browse: NonNullable<EngineMock["browse"]> = mock.browse;
export const apply: NonNullable<EngineMock["apply"]> = mock.apply;
export const run: NonNullable<EngineMock["run"]> = mock.run;
export const invoke: NonNullable<EngineMock["invoke"]> = mock.invoke;
export const classify = classifyConsole;
