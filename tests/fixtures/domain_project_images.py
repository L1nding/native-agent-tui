"""Read only image admission metadata from the exact dsh session used by a fixture."""

import json
import subprocess
from pathlib import Path

NODE_READER = r"""
const fs=require('node:fs'),z=require('node:zlib');
const bytes=fs.readFileSync(process.argv[1]);
if(bytes.length>8*1024*1024)throw Error('fixture session exceeds compressed read bound');
let position=0,total=0,rows=[];
while(position<bytes.length){
 const result=z.zstdDecompressSync(bytes.subarray(position),{info:true});
 if(result.engine.bytesWritten<=0)throw Error('invalid compressed session frame');
 position+=result.engine.bytesWritten;total+=result.buffer.length;
 if(total>32*1024*1024)throw Error('fixture session exceeds decoded read bound');
 rows.push(...result.buffer.toString().trim().split('\n').filter(Boolean).map(JSON.parse));
}
const admitted=[],errors=[];
for(const row of rows){
 if(row.type!=='tool/result')continue;
 const receipts=[],images=[];
 function visit(value,depth=0){
  if(depth>16)return;
  if(Array.isArray(value))value.forEach(item=>visit(item,depth+1));
  else if(value&&typeof value==='object'){
   if(value.type==='image'&&typeof value.attachment?.attachmentId==='string'){
    images.push({attachmentId:value.attachment.attachmentId,mediaType:value.attachment.mediaType,
                 width:value.attachment.width,height:value.attachment.height,offloaded:value.offloaded===true});
   }
   if(value.type==='text'&&typeof value.text==='string'){
    const diagnostic=value.text.match(/\[image unavailable:[^\]]+\]/);
    if(diagnostic)errors.push(diagnostic[0].slice(0,240));
    if(value.text.length<=65536){try{
     const receipt=JSON.parse(value.text);
     if(receipt.contract==='modeling-fact-receipt/v1')receipts.push(receipt.fact_sha256);
    }catch{}}
   }
   Object.values(value).forEach(item=>{if(item&&typeof item==='object')visit(item,depth+1);});
  }
 }
 visit(row.data);
 const laterResponse=rows.find(next=>next.type==='assistant/message'&&next.seq>row.seq&&
   next.data?.turn===row.data?.turn&&next.data?.step>row.data?.step);
 const unique=[...new Set(receipts)];
 if(unique.length===1&&images.length&&laterResponse){
  admitted.push({fact_sha256:unique[0],images,tool_result_seq:row.seq,subsequent_model_step:laterResponse.data.step,subsequent_assistant_seq:laterResponse.seq});
 }
}
console.log(JSON.stringify({admitted,errors:[...new Set(errors)]}));
"""


def admitted_images(journal, expected_receipts, *, session_root=None):
    sessions = set()
    for path in Path(journal).glob("*.jsonl"):
        for line in path.read_text(encoding="utf-8").splitlines():
            for task in json.loads(line).get("payload", {}).get("tasks", []):
                identity = task.get("external") or {}
                if task.get("kind") == "rootTurn" and identity.get("thread_id"):
                    sessions.add(identity["thread_id"])
    if len(sessions) != 1:
        raise AssertionError("fixture journal does not identify one dsh session")
    session = next(iter(sessions))
    root = (
        Path(session_root)
        if session_root is not None
        else Path.home() / ".dsh/sessions"
    )
    paths = list(root.glob(f"*/{session}/session.v4.jsonl.zstd"))
    if len(paths) != 1:
        raise AssertionError("exact durable dsh session was not found")
    result = subprocess.run(
        ["node", "-e", NODE_READER, str(paths[0])],
        capture_output=True,
        text=True,
        check=True,
    )
    metadata = json.loads(result.stdout)
    verified = {
        row["fact_sha256"]: row
        for row in metadata["admitted"]
        if all(not image["offloaded"] for image in row["images"])
    }
    if not set(expected_receipts) <= set(verified):
        raise AssertionError(
            "actual images were not admitted to a subsequent model request: "
            + "; ".join(metadata["errors"])
        )
    return {
        "session_id": session,
        "admitted": [verified[key] for key in expected_receipts],
    }
