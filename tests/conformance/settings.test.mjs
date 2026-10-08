import assert from 'node:assert/strict';
import { test } from 'node:test';
const base=process.env.PITCREW_CONFORMANCE_URL;
const person=process.env.PITCREW_CONFORMANCE_PERSON;
const agent=process.env.PITCREW_CONFORMANCE_AGENT;
const second=process.env.PITCREW_CONFORMANCE_SECOND_PERSON;
async function request(method,path,status,body,token=person) {
  const response=await fetch(new URL(path,base),{method,headers:{Authorization:`Bearer ${token}`,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(30000)});
  const value=await response.json(); assert.equal(response.status,status,JSON.stringify(value)); return value;
}
test('settings: ownership, validation, exact reads and authored profile/recipe changes',async()=>{
  const me=await request('GET','/v1/me',200);
  const personas=await request('GET','/v1/personas',200);
  const recipe=personas[0]; assert.ok(recipe);
  const machines=await request('GET','/v1/machines',200);
  const machine=machines[0]; assert.ok(machine);
  await request('PUT',`/v1/machines/${machine.id}`,403,{name:'Updated machine'},agent);
  await request('PUT',`/v1/machines/${machine.id}`,403,{name:'Updated machine'},second);
  await request('PUT','/v1/machines/00000000000000000000000000',404,{name:'Updated machine'});
  await request('PUT',`/v1/machines/${machine.id}`,400,{name:'x'.repeat(61)});
  await request('PUT',`/v1/machines/${machine.id}`,400,{name:'Updated machine',extra:true});
  const updatedMachine=await request('PUT',`/v1/machines/${machine.id}`,200,{name:' Updated machine '}); assert.equal(updatedMachine.name,'Updated machine');
  assert.deepEqual((await request('GET','/v1/machines',200)).find(m=>m.id===machine.id),updatedMachine);
  const profile={name:'Sam Updated',handle:'@sam-updated',avatar:{initials:'SU',colour:'#abcdef'}};
  const defaults={name:'Updated recipe',engine:'codex',model:'synthetic-model',instructions:'Synthetic instructions',permission_mode:'plan'};
  for(const [method,path,body] of [['GET','/v1/settings'],['PUT','/v1/settings/workspace',{name:'Workspace'}],['PUT','/v1/me/profile',profile],['PUT',`/v1/personas/${recipe.id}`,defaults]]) {
    await request(method,path,403,body,agent); await request(method,path,401,body,'invalid-synthetic-token');
  }
  for(const [method,path,body] of [['GET','/v1/settings'],['PUT','/v1/settings/workspace',{name:'Workspace'}],['PUT',`/v1/personas/${recipe.id}`,defaults]]) await request(method,path,403,body,second);
  const info=await request('GET','/v1/settings',200); assert.equal(info.owner,me.id); assert.equal(typeof info.data_folder,'string'); assert.equal(typeof info.logs,'string'); assert.equal(typeof info.daemon_version,'string'); assert.equal(info.protocol_version,1);
  await request('PUT','/v1/settings/workspace',400,{name:'',extra:true});
  await request('PUT','/v1/settings/workspace',400,{name:'x'.repeat(81)});
  const renamed=await request('PUT','/v1/settings/workspace',200,{name:' Renamed workspace '}); assert.equal(renamed.name,'Renamed workspace'); assert.equal((await request('GET','/v1/workspace',200)).workspace.name,renamed.name);
  for(const body of [{...profile,name:''},{...profile,handle:'bad'},{...profile,handle:'@office'},{...profile,extra:true},{...profile,avatar:{initials:'ABCDE',colour:'#abcdef'}},{...profile,avatar:{initials:'SU',colour:'red'}},{...profile,avatar:{initials:'SU',colour:'#abcdef',extra:true}}]) await request('PUT','/v1/me/profile',400,body);
  const members=await request('GET','/v1/members',200); const other=members.find(m=>m.id!==me.id); await request('PUT','/v1/me/profile',409,{...profile,handle:other.handle});
  const saved=await request('PUT','/v1/me/profile',200,profile); assert.equal(saved.id,me.id); assert.deepEqual(saved.avatar,profile.avatar); assert.deepEqual(await request('GET','/v1/me',200),saved);
  const rev=(await request('GET','/v1/workspace',200)).rev; await request('PUT','/v1/me/profile',200,profile); assert.equal((await request('GET','/v1/workspace',200)).rev,rev);
  await request('PUT','/v1/personas/00000000000000000000000000',404,defaults);
  for(const body of [{...defaults,extra:true},{...defaults,name:''},{...defaults,model:'x'.repeat(201)},{...defaults,permission_mode:'bypass_permissions'}]) await request('PUT',`/v1/personas/${recipe.id}`,400,body);
  const changed=await request('PUT',`/v1/personas/${recipe.id}`,200,defaults); assert.equal(changed.id,recipe.id); assert.deepEqual((await request('GET','/v1/personas',200)).find(p=>p.id===recipe.id),changed);
});
