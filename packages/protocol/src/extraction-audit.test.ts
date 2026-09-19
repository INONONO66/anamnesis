import {test,expect} from 'bun:test';
import {CreateExtractionPipeline,ExtractionPipeline,ExtractionDisposition} from './extraction-audit.ts';
import {ExtractionModelOutput,CreateModelTask} from './extraction.ts';
import {RpcRequest,RpcCapabilities} from './rpc.ts';
const id='01900000-0000-7000-8000-000000000001';
const evidence={start:1,end:7,text:'é🙂'};
const output={task:'judge_claims',claim_body_digest:'a'.repeat(64),decisions:[{claim_index:0,disposition:'unknown',evidence}],language:'und',modality:'text'};
test('audit ABI has bounded strict decisions, explicit unknown and no semantic authorization',()=>{
 expect(ExtractionModelOutput.safeParse(output).success).toBe(true);
 for(const body of [{...output,target_ids:[id]},{...output,decisions:Array(65).fill(output.decisions[0])},{...output,decisions:[{...output.decisions[0],claim_index:64}]},{...output,decisions:[{...output.decisions[0],disposition:'invalidate'}]}])expect(ExtractionModelOutput.safeParse(body).success).toBe(false);
 expect(ExtractionPipeline.parse({state:'unknown',pipeline_id:id})).toEqual({state:'unknown',pipeline_id:id});
 expect(ExtractionPipeline.safeParse({state:'unknown',pipeline_id:id,semantic_writes:true}).success).toBe(false);
 expect(ExtractionDisposition.safeParse({judge_attempt_id:id,claim_attempt_id:id,claim_body_digest:'a'.repeat(64),claim_index:0,disposition:'correct',evidence}).success).toBe(true);
});
test('known pipeline responses carry a strict materialization boolean',()=>{
 const marker=ExtractionPipeline.options[1].shape.semantic_writes;
 expect(marker.safeParse(true).success).toBe(true);
 expect(marker.safeParse(false).success).toBe(true);
 expect(marker.safeParse('true').success).toBe(false);
});
test('admission accepts IDs only; models, claims, sources and policy are not caller replacements',()=>{
 const params={id,generation_id:id,source_id:id};expect(CreateExtractionPipeline.parse(params)).toEqual(params);
 for(const field of ['model','policy_revision','claim_context','body_digest','origin_role','lineage_mode'])expect(CreateExtractionPipeline.safeParse({...params,[field]:'forged'}).success).toBe(false);
 expect(CreateModelTask.safeParse({...params,kind:'judge',model:'fixture',model_incarnation:'a'.repeat(64),pipeline:'claim-judge-audit-v1'}).success).toBe(false);
});
test('authenticated dispatcher has distinct audit methods; extraction capability is a strict provider-backed boolean',()=>{
 for(const [method,params]of [['extraction.audit.create',{id,generation_id:id,source_id:id}],['extraction.audit.run',{task_id:id,expected_version:0,worker_id:'qa',lease_ms:30000}],['extraction.audit.status',{pipeline_id:id}]]as const)expect(RpcRequest.safeParse({jsonrpc:'2.0',id:1,method,params}).success).toBe(true);
 expect(RpcCapabilities.shape.extraction.safeParse(true).success).toBe(true);
 expect(RpcCapabilities.shape.extraction.safeParse(false).success).toBe(true);
 expect(RpcCapabilities.shape.extraction.safeParse('enabled').success).toBe(false);
 for(const method of ['extraction.materialize','derived.recall'])expect(RpcRequest.safeParse({jsonrpc:'2.0',id:1,method,params:{}}).success).toBe(false);
});
