#!/usr/bin/env python3
"""Standalone Strand schema compiler, adapted from the Tondi v10 compiler. No external codec or code generator."""
import argparse,json,pathlib,re
SCHEMA = {}
TS = {}
EMIT_FRAMES = False
EMIT_INDEXES = True
def fixed(n):
 if n=='byte':return 1
 t=TS[n];k=t['kind']
 if k=='fixed':return t['size']
 if k=='struct':
  sizes=[fixed(f['type']) for f in t['fields']]
  return sum(sizes) if all(x is not None for x in sizes) else None
 return None
def rt(n):return 'Byte' if n=='byte' else n
def rr(n):return 'ByteReader' if n=='byte' else n+'Reader'
def frame_ref(n):
 return 'crate::strand_transport::Frame::Fixed(1)' if n=='byte' else n+'::FRAME'
def frame(n):
 t=TS[n];k=t['kind'];F='crate::strand_transport::Frame::'
 if k in ('fixed','struct'):return F+f'Fixed({fixed(n)})'
 if k=='sequence':return F+'Sequence(&['+','.join(frame_ref(f['type']) for f in t['fields'])+'])'
 if k=='flagged':return F+'Flagged{width:'+str(t['flag_bytes'])+',fields:&['+','.join('('+('Some('+str(f['bit'])+')' if 'bit' in f else 'None')+','+frame_ref(f['type'])+')' for f in t['fields'])+']}'
 if k=='optional':return frame_ref(t['item'])
 if k=='bytes':return F+'Bytes'
 if k=='uleb':return F+f'Uleb({t["bits"]})'
 if k=='union':return F+'Union{width:'+str(t.get('tag_bytes',1))+',variants:&['+','.join(frame_ref(v) for v in t['variants'])+']}'
 if k=='dense_vector':return F+'DenseVec{item:&'+frame_ref(t['item'])+',max_items:crate::codec::MAX_OBJECT_BYTES}'
 raise ValueError('unknown frame kind: '+k)
def default_bytes(n):
 if n=='byte':return bytes(1)
 t=TS[n];k=t['kind']
 if k=='fixed':return bytes([t['allowed_values'][0]]) if 'allowed_values' in t else bytes(t['size'])
 if k=='optional':return b''
 if k=='flagged':return bytes(t['flag_bytes'])+b''.join(default_bytes(f['type']) for f in t['fields'] if 'bit' not in f)
 if k=='uleb':return bytes(1)
 if k=='bytes':return bytes(4)
 if k=='dense_vector':return bytes(4)
 if k=='union':return bytes(t.get('tag_bytes',1))+default_bytes(t['variants'][0])
 fields=[default_bytes(f['type']) for f in t['fields']]
 return b''.join(fields)

def minimum_size(n):
 if n=='byte':return 1
 t=TS[n];k=t['kind']
 if k=='fixed':return t['size']
 if k=='optional':return 0
 if k=='flagged':return t['flag_bytes']+sum(minimum_size(f['type']) for f in t['fields'] if 'bit' not in f)
 if k=='uleb':return 1
 if k in ['bytes','dense_vector']:return 4
 if k=='union':return t.get('tag_bytes',1)+min(minimum_size(v) for v in t['variants'])
 return sum(minimum_size(f['type']) for f in t['fields'])
def delimited(n):
 if n=='byte':return True
 t=TS[n];k=t['kind']
 if k=='optional':return False
 if k=='union':return all(delimited(v) for v in t['variants'])
 if k=='sequence':return all(delimited(f['type']) for f in t['fields'])
 return True
def validate_schema():
 def visit(n,stack):
  if n=='byte':return
  if n in stack:raise ValueError('recursive Strand type: '+n)
  t=TS[n];children=[f['type'] for f in t.get('fields',[])]+t.get('variants',[])+([t['item']] if 'item' in t else [])
  if t['kind'] not in ('fixed','struct','sequence','flagged','bytes','dense_vector','union','uleb','optional'):raise ValueError('unsupported Strand v1 primitive: '+t['kind'])
  if t['kind']=='uleb' and t.get('bits') not in (16,32,64,65,128):raise ValueError('unsupported ULEB width: '+n)
  if t['kind']=='union' and (t.get('tag_bytes',1)!=1 or not t['variants'] or len(t['variants'])>256):raise ValueError('Strand v1 requires closed one-byte union tags: '+n)
  if 'allowed_values' in t:
   values=t['allowed_values']
   if t['kind']!='fixed' or t['size']!=1 or not values or any(type(v) is not int or v<0 or v>255 for v in values) or values!=sorted(set(values)):raise ValueError('closed byte requires ordered unique values: '+n)
  for child in children:visit(child,stack+[n])
  for f in t.get('fields',[]):
   if f['name'].startswith('__tg_'):raise ValueError('reserved Reader field name: '+f['name'])
   if TS.get(f['type'],{}).get('kind')=='optional' and (t['kind']!='flagged' or 'bit' not in f):raise ValueError('Option requires a parent presence bit: '+n+'.'+f['name'])
  if t['kind']=='flagged':
   width=t.get('flag_bytes');bits=[f['bit'] for f in t['fields'] if 'bit' in f]
   if width not in (1,2) or len(bits)!=len(set(bits)) or any(type(b) is not int or b<0 or b>=8*width for b in bits) or not all(delimited(TS[f['type']]['item']) if 'bit' in f and TS[f['type']]['kind']=='optional' else delimited(f['type']) for f in t['fields']) or any(TS[f['type']]['kind'] not in ('bytes','dense_vector','optional') for f in t['fields'] if 'bit' in f):raise ValueError('invalid flagged record: '+n)
  if t['kind']=='struct' and fixed(n) is None:raise ValueError('variable field in fixed struct: '+n)
  if t['kind']=='sequence' and not delimited(n):raise ValueError('sequence needs self-delimiting fields: '+n)
  if t['kind']=='union' and not delimited(n):raise ValueError('union requires self-delimiting payloads: '+n)
  if t['kind']=='optional' and (minimum_size(t['item'])==0 or not delimited(t['item'])):raise ValueError('optional payload must be nonempty and self-delimiting: '+n)
  if t['kind']=='dense_vector' and (not delimited(t['item']) or minimum_size(t['item'])==0):raise ValueError('dense vector needs nonempty self-delimiting items: '+n)
 for n in TS:visit(n,[])
def rust():
 out=['// Generated by Stem Strand. Edit the schema, then regenerate.','use crate::strand::{self as tg,prelude::*,VerificationError};','extern crate alloc;','use alloc::vec::Vec;']
 for n,t in TS.items():
  k=t['kind'];fields=t.get('fields',[]);items=t.get('variants',[]);child=t.get('item','byte' if k=='bytes' else None);width=fixed(n)
  verify='';span='';reader='';bfields='';bdefault='';bmethods='';write='';build='';asbuilder=''
  if k=='fixed':
   verify=f'if b.len()!={width}{{return Err(VerificationError)}}Ok(())';span=f'if b.len()<{width}{{Err(VerificationError)}}else{{Ok({width})}}'
   bfields='items:Vec<Byte>';bdefault=f'items:alloc::vec![Byte::default();{width}]';bmethods='pub fn set(mut self,v:Vec<Byte>)->Self{self.items=v;self}'
   build=f'assert_eq!(self.items.len(),{width}); let data=self.items.iter().map(|v|v.as_slice()[0]).collect::<Vec<_>>();';write='for v in &self.items{w.write_all(v.as_slice())?;}Ok(())';asbuilder='Self::Builder{items:self.as_slice().iter().copied().map(Into::into).collect()}'
   if 'allowed_values' in t:
    verify='if b.len()!=1 || !matches!(b[0],'+ '|'.join(str(v) for v in t['allowed_values']) +'){return Err(VerificationError)}Ok(())'
    bdefault=f'items:alloc::vec![{t["allowed_values"][0]}u8.into()]'
  elif k in ['struct','sequence']:
   count=len(fields);bfields=','.join(f"{f['name']}:{rt(f['type'])}" for f in fields);bdefault=','.join(f"{f['name']}:Default::default()" for f in fields)
   bmethods=''.join(f"pub fn {f['name']}(mut self,v:{rt(f['type'])})->Self{{self.{f['name']}=v;self}}" for f in fields)
   verify='let mut pos=0usize;'
   for f in fields:verify+=f'let len={rr(f["type"])}::span(b.get(pos..).ok_or(VerificationError)?)?;{rr(f["type"])}::verify(&b[pos..pos+len])?;pos+=len;'
   verify+='if pos!=b.len(){return Err(VerificationError)}Ok(())'
   span='let mut pos=0usize;'+''.join(f'pos+={rr(f["type"])}::span(b.get(pos..).ok_or(VerificationError)?)?;' for f in fields)+'Ok(pos)'
   for i,f in enumerate(fields):
    if k=='struct':
     start=sum(fixed(prev['type']) for prev in fields[:i]);end=start+fixed(f['type'])
     reader+=f'pub fn {f["name"]}(&self)->{rr(f["type"])}<\'r>{{{rr(f["type"])}::new_unchecked(&self.0[{start}..{end}])}}'
    else:
     pre='let mut pos=0;'+''.join(f'pos+={rr(prev["type"])}::span(&self.0[pos..]).expect("verified field");' for prev in fields[:i])
     reader+=f'pub fn {f["name"]}(&self)->{rr(f["type"])}<\'r>{{{pre}let end=pos+{rr(f["type"])}::span(&self.0[pos..]).expect("verified field");{rr(f["type"])}::new_unchecked(&self.0[pos..end])}}'
   if k=='sequence':
    index=f'fn __tg_spans(b:&[u8])->Result<[u32;{count+1}],VerificationError>{{let mut offsets=[0;{count+1}];let mut pos=0usize;'
    for i,f in enumerate(fields):index+=f'pos=pos.checked_add({rr(f["type"])}::span(b.get(pos..).ok_or(VerificationError)?)?).ok_or(VerificationError)?;offsets[{i+1}]=u32::try_from(pos).map_err(|_|VerificationError)?;'
    index+='Ok(offsets)}'
    reader=index+''.join(f'pub fn {f["name"]}(&self)->{rr(f["type"])}<\'r>{{{rr(f["type"])}::new_unchecked(&self.0[self.1[{i}] as usize..self.1[{i+1}] as usize])}}' for i,f in enumerate(fields))
    span=f'Ok(Self::__tg_spans(b)?[{count}] as usize)'
   build='let mut data=Vec::with_capacity('+('+'.join(f'self.{f["name"]}.as_slice().len()' for f in fields) or '0')+');'+''.join(f'data.extend_from_slice(self.{f["name"]}.as_slice());' for f in fields)
   write=''.join(f'w.write_all(self.{f["name"]}.as_slice())?;' for f in fields)+'Ok(())'
   asbuilder='Self::Builder{'+','.join(f'{f["name"]}:{rt(f["type"])}::new_unchecked(tg::Bytes::copy_from_slice(self.as_reader().{f["name"]}().as_slice()))' if f['type']!='byte' else f'{f["name"]}:self.as_reader().{f["name"]}().as_slice()[0].into()' for f in fields)+'}'
  elif k=='flagged':
   count=len(fields);width=t['flag_bytes'];mask=sum(1<<f['bit'] for f in fields if 'bit' in f)
   read_flags='*b.first().ok_or(VerificationError)? as u16' if width==1 else 'u16::from_le_bytes(b.get(..2).ok_or(VerificationError)?.try_into().map_err(|_|VerificationError)?)'
   index=f'fn __tg_spans(b:&[u8])->Result<([u32;{count+1}],usize),VerificationError>{{let flags={read_flags};if flags & !{mask}u16!=0{{return Err(VerificationError)}}let mut offsets=[{width};{count+1}];let mut pos={width};'
   verify='let (offsets,pos)=Self::__tg_spans(b)?;if pos!=b.len(){return Err(VerificationError)}'
   bfields=','.join(f"{f['name']}:{rt(f['type'])}" for f in fields);bdefault=','.join(f"{f['name']}:Default::default()" for f in fields);bmethods=''.join(f"pub fn {f['name']}(mut self,v:{rt(f['type'])})->Self{{self.{f['name']}=v;self}}" for f in fields)
   flag_build='let mut flags=0u16;';append='';write_body='';reader=''
   for i,f in enumerate(fields):
    field=f['name'];ty=f['type'];optional='bit' in f;default='(&['+','.join(str(b)+'u8' for b in default_bytes(ty))+'] as &[u8])';condition=f"flags & {1<<f['bit']} !=0" if optional else 'true'
    span_type=TS[ty]['item'] if optional and TS[ty]['kind']=='optional' else ty
    index+=f'if {condition}{{let len={rr(span_type)}::span(b.get(pos..).ok_or(VerificationError)?)?;pos+=len;}}offsets[{i+1}]=u32::try_from(pos).map_err(|_|VerificationError)?;'
    verify+=f'if offsets[{i+1}]!=offsets[{i}]{{let field=&b[offsets[{i}] as usize..offsets[{i+1}] as usize];{rr(ty)}::verify(field)?;'
    if optional:verify+=f'if field=={default}{{return Err(VerificationError)}}'
    verify+='}'
    reader+=f"pub fn {field}(&self)->{rr(ty)}<'r>{{let start=self.1[{i}] as usize;let len=(self.1[{i+1}]-self.1[{i}]) as usize;{rr(ty)}::new_unchecked("+(f'if len==0{{{default}}}else{{&self.0[start..start+len]}}' if optional else '&self.0[start..start+len]')+')}'
    if optional:flag_build+=f"if self.{field}.as_slice()!={default}{{flags|={1<<f['bit']};}}"
    append+=f'if {condition}{{data.extend_from_slice(self.{field}.as_slice());}}';write_body+=f'if {condition}{{w.write_all(self.{field}.as_slice())?;}}'
   index+='Ok((offsets,pos))}';reader=index+reader;verify+='Ok(())';span='Ok(Self::__tg_spans(b)?.1)'
   flag_bytes='&[flags as u8]' if width==1 else '&flags.to_le_bytes()'
   build=flag_build+'let mut data=Vec::new();data.extend_from_slice('+flag_bytes+');'+append
   write=flag_build+'w.write_all('+flag_bytes+')?;'+write_body+'Ok(())'
   asbuilder='Self::Builder{'+','.join(f"{f['name']}:{rt(f['type'])}::new_unchecked(tg::Bytes::copy_from_slice(self.as_reader().{f['name']}().as_slice()))" if f['type']!='byte' else f"{f['name']}:self.as_reader().{f['name']}().as_slice()[0].into()" for f in fields)+'}'
  elif k in ['bytes','dense_vector']:
   w=fixed(child);bfields=f'items:Vec<{rt(child)}>';bdefault='items:Vec::new()';bmethods=f'pub fn set(mut self,v:Vec<{rt(child)}>)->Self{{self.items=v;self}}'
   if k=='dense_vector' and not w:
    verify='let n=tg::number(b)?;if n>b.len().saturating_sub(4){return Err(VerificationError)}let mut pos=4;for _ in 0..n{let len='+rr(child)+'::span(&b[pos..])?;'+rr(child)+'::verify(&b[pos..pos+len])?;pos+=len;}if pos!=b.len(){return Err(VerificationError)}Ok(())'
    span='let n=tg::number(b)?;if n>b.len().saturating_sub(4){return Err(VerificationError)}let mut pos=4;for _ in 0..n{pos+='+rr(child)+'::span(&b[pos..])?;}Ok(pos)'
    get='let mut pos=4;for _ in 0..index{pos+='+rr(child)+'::span(&self.0[pos..]).ok()?;}let end=pos+'+rr(child)+'::span(&self.0[pos..]).ok()?;Some('+rr(child)+'::new_unchecked(&self.0[pos..end]))';length='tg::unpack_number(self.0) as usize'
    head='let mut data=Vec::new();data.extend_from_slice(&(self.items.len() as u32).to_le_bytes());'
   elif w:
    verify=f'tg::fixed_vector_check(b,{w})?;Ok(())';span=f'let n=tg::number(b)?.checked_mul({w}).and_then(|v|v.checked_add(4)).ok_or(VerificationError)?;if n>b.len(){{Err(VerificationError)}}else{{Ok(n)}}'
    get=f'let start=4+{w}*index;Some({rr(child)}::new_unchecked(&self.0[start..start+{w}]))';length='tg::unpack_number(self.0) as usize'
    head='let mut data=Vec::new();data.extend_from_slice(&(self.items.len() as u32).to_le_bytes());'
   reader=f'pub fn len(&self)->usize{{{length}}}pub fn is_empty(&self)->bool{{self.len()==0}}pub fn get(&self,index:usize)->Option<{rr(child)}<\'r>>{{if index>=self.len(){{return None}}{get}}}pub fn iter(&self)->impl ExactSizeIterator<Item={rr(child)}<\'r>>+\'r{{let copy=*self;(0..self.len()).map(move|i|copy.get(i).expect("verified vector"))}}'
   if k=='dense_vector':
    begin=reader.index('pub fn iter(')
    reader=reader[:begin]+f'pub fn iter(&self)->impl ExactSizeIterator<Item={rr(child)}<\'r>>+\'r{{let copy=*self;let mut pos=4;(0..self.len()).map(move|_|{{let len={rr(child)}::span(&copy.0[pos..]).expect("verified dense item");let value={rr(child)}::new_unchecked(&copy.0[pos..pos+len]);pos+=len;value}})}}'
   if k=='dense_vector' and EMIT_INDEXES:reader+=f'pub fn indexed<const MAX:usize>(&self)->Result<tg::DenseIndex<\'r,{rr(child)}<\'r>,MAX>,VerificationError>{{tg::DenseIndex::new(self.0)}}'
   if child=='byte':reader+='pub fn raw_data(&self)->&\'r[u8]{&self.0[4..]}'
   build=head.replace('Vec::new()', 'Vec::with_capacity(4+'+'self.items.iter().map(|v|v.as_slice().len()).sum::<usize>())')+'for v in &self.items{data.extend_from_slice(v.as_slice());}';write=head+'w.write_all(&data)?;for v in &self.items{w.write_all(v.as_slice())?;}Ok(())'
   asbuilder='Self::Builder{items:self.as_reader().iter().map(|v|'+(child+'::new_unchecked(tg::Bytes::copy_from_slice(v.as_slice()))' if child!='byte' else 'v.as_slice()[0].into()')+').collect()}'
  elif k=='optional':
   verify='if b.is_empty(){Ok(())}else{'+rr(child)+'::verify(b)}';span='if b.is_empty(){Ok(0)}else{'+rr(child)+'::span(b)}';reader=f'pub fn to_opt(&self)->Option<{rr(child)}<\'r>>{{if self.0.is_empty(){{None}}else{{Some({rr(child)}::new_unchecked(self.0))}}}}'
   bfields=f'item:Option<{child}>';bdefault='item:None';bmethods=f'pub fn set(mut self,v:Option<{child}>)->Self{{self.item=v;self}}';build='let data=self.item.map(|v|v.as_slice().to_vec()).unwrap_or_default();';write='if let Some(v)=&self.item{w.write_all(v.as_slice())?;}Ok(())';asbuilder='Self::Builder{item:self.as_reader().to_opt().map(|v|'+child+'::new_unchecked(tg::Bytes::copy_from_slice(v.as_slice())))}'
  elif k=='uleb':
   bits=t['bits'];verify=f'if tg::uleb_span(b,{bits})?!=b.len(){{Err(VerificationError)}}else{{Ok(())}}';span=f'tg::uleb_span(b,{bits})';reader='pub fn value(&self)->u128{tg::uleb_value(self.0)}';bfields='value:u128';bdefault='value:0';bmethods='pub fn set(mut self,v:u128)->Self{self.value=v;self}'
   checked=f'assert!(self.value<=(1u128<<{bits})-1,"ULEB schema width");' if bits<128 else ''
   build=checked+'let data=tg::uleb_encode(self.value);';write=checked+'w.write_all(&tg::uleb_encode(self.value))';asbuilder='Self::Builder{value:self.as_reader().value()}'
  elif k=='union':
   tag=t.get('tag_bytes',1);tag_read='*b.first().ok_or(VerificationError)? as usize' if tag==1 else 'tg::number(b)?';tag_value='self.0[0] as u32' if tag==1 else 'tg::unpack_number(self.0)';tag_write='&[self.item.item_id() as u8]' if tag==1 else '&self.item.item_id().to_le_bytes()'
   out+=['#[derive(Clone,Debug)]pub enum '+n+'Union{'+','.join(f'{v}({v})' for v in items)+'}', '#[derive(Clone,Copy,Debug)]pub enum '+n+'UnionReader<\'r>{'+','.join(f'{v}({v}Reader<\'r>)' for v in items)+'}']
   for v in items:out += [f'impl From<{v}> for {n}Union{{fn from(v:{v})->Self{{Self::{v}(v)}}}}']
   out += [f'impl {n}Union{{pub fn as_slice(&self)->&[u8]{{match self{{'+','.join(f'Self::{v}(x)=>x.as_slice()' for v in items)+'}}pub fn item_id(&self)->u32{match self{'+','.join(f'Self::{v}(_)=>{i}' for i,v in enumerate(items))+'}}}',f'impl<\'r> {n}UnionReader<\'r>{{pub fn as_slice(&self)->&\'r[u8]{{match self{{'+','.join(f'Self::{v}(x)=>x.as_slice()' for v in items)+'}}}']
   verify='let tag='+tag_read+';match tag{'+','.join(f'{i}=>{v}Reader::verify(&b[{tag}..])' for i,v in enumerate(items))+',_=>Err(VerificationError)}';span='let tag='+tag_read+f';Ok({tag}+match tag{{'+','.join(f'{i}=>{v}Reader::span(&b[{tag}..])?' for i,v in enumerate(items))+',_=>return Err(VerificationError)})'
   reader='pub fn to_enum(&self)->'+n+'UnionReader<\'r>{match '+tag_value+'{'+','.join(f'{i}=>{n}UnionReader::{v}({v}Reader::new_unchecked(&self.0[{tag}..]))' for i,v in enumerate(items))+',_=>unreachable!("verified union")}}'
   for f,ty in t.get('projection',{}).items():
    arms=[]
    for v in items:
     present=any(x['name']==f for x in TS[v]['fields']);expr=f'x.{f}()' if present else f'{ty}Reader::new_unchecked(&tg::EMPTY_VECTOR)';arms.append(f'{n}UnionReader::{v}(x)=>{expr}')
    reader+=f'pub fn {f}(&self)->{ty}Reader<\'r>{{match self.to_enum(){{'+','.join(arms)+'}}'
   bfields=f'item:{n}Union';bdefault=f'item:{n}Union::{items[0]}({items[0]}::default())';bmethods=f'pub fn set(mut self,v:impl Into<{n}Union>)->Self{{self.item=v.into();self}}';build=f'let mut data=Vec::with_capacity({tag}+self.item.as_slice().len());data.extend_from_slice('+tag_write+');data.extend_from_slice(self.item.as_slice());';write='w.write_all('+tag_write+')?;w.write_all(self.item.as_slice())';asbuilder='Self::Builder{item:match self.as_reader().to_enum(){'+','.join(f'{n}UnionReader::{v}(r)=>{n}Union::{v}({v}::new_unchecked(tg::Bytes::copy_from_slice(r.as_slice())))' for v in items)+'}}'
  # Schema semantic canonicality that must match in both generated languages.
  c=t.get('constraint')
  if c:
   check={'asset_group':'let r=Self::new_unchecked(b);let e=r.entries();if e.is_empty()||e.len()>64{return Err(VerificationError)}let mut previous:Option<&[u8]>=None;for a in e.iter(){let name=a.name().raw_data();if name.len()>32||a.amount().value()==0||previous.is_some_and(|p|p>=name){return Err(VerificationError)}previous=Some(name);}', 'asset_groups':'let r=Self::new_unchecked(b);if r.len()>64{return Err(VerificationError)}let mut previous:Option<&[u8]>=None;let mut count=0;for group in r.iter(){let policy=group.policy().as_slice();if previous.is_some_and(|p|p>=policy){return Err(VerificationError)}previous=Some(policy);count+=group.entries().len();}if count>64{return Err(VerificationError)}'}.get(c,'')
   if c=='proof_directory':check='let r=Self::new_unchecked(b);let mut previous:Option<&[u8]>=None;for p in r.iter(){let id=p.obligation().as_slice();if previous.is_some_and(|old|old>=id){return Err(VerificationError)}previous=Some(id);}'
   if c=='shared_authorizations':check="""let r=Self::new_unchecked(b);let owned=r.body().owned();let auth=r.owned_authorizations();if owned.len()!=auth.len(){return Err(VerificationError)}let mut previous:Option<(&[u8],u32)>=None;for input in owned.iter(){let p=input.outpoint();let hash=p.transaction_id().as_slice();let index=u32::from_le_bytes(p.index().as_slice().try_into().map_err(|_|VerificationError)?);if previous.is_some_and(|old|old>=(hash,index)){return Err(VerificationError)}previous=Some((hash,index));}if let Some(gas)=r.gas_authorization().to_opt(){match gas.script_ref().to_enum(){ScriptRefUnionReader::ScriptIndex(p)=>{let index=p.index().value() as usize;let script=auth.get(index).ok_or(VerificationError)?.script();if auth.iter().take(index).any(|a|a.script().as_slice()==script.as_slice()){return Err(VerificationError)}},ScriptRefUnionReader::AuthProgramme(script)=>{if auth.iter().any(|a|a.script().as_slice()==script.as_slice()){return Err(VerificationError)}}}}"""
   if not check:raise ValueError('unknown canonical constraint: '+c)
   verify='let structural:tg::Verify=(||{'+verify+'})();structural?;'+check.replace('Self::new_unchecked(b)',n+'Reader::new_unchecked(b)')+'Ok(())'
  names='&['+','.join(json.dumps(f['name']) for f in fields)+']'
  prefix=''
  ioerror='std::io::Error::new(std::io::ErrorKind::InvalidInput,"Strand length overflow")'
  if k in ['struct','sequence','flagged']:
   presence=','.join(f'sizes[{i}]!={len(default_bytes(f["type"]))}' if 'bit' in f else 'true' for i,f in enumerate(fields))
   prefix=f'pub fn present_fields(sizes:[usize;{len(fields)}])->[bool;{len(fields)}]{{let _=sizes;[{presence}]}}'
   prefix+=f'pub fn encoded_len(sizes:[usize;{len(fields)}])->usize{{Self::present_fields(sizes).into_iter().zip(sizes).filter_map(|(present,size)|present.then_some(size)).fold(Self::HEADER_SIZE,usize::saturating_add)}}'
   prefix+=f'pub fn field_spans(sizes:[usize;{len(fields)}])->[core::ops::Range<usize>;{len(fields)}]{{let present=Self::present_fields(sizes);let mut position=Self::HEADER_SIZE;core::array::from_fn(|i|{{let start=position;if present[i]{{position=position.saturating_add(sizes[i]);}}start..position}})}}'
   prefix+=f'#[cfg(not(target_os="none"))]pub fn write_prefix<W:std::io::Write>(sizes:[usize;{len(fields)}],w:&mut W)->std::io::Result<()>{{'

   if k=='flagged':
    prefix+='let present=Self::present_fields(sizes);let mut flags=0u16;'
    for i,f in enumerate(fields):
     if 'bit' in f:prefix+=f'if present[{i}]{{flags|={1<<f["bit"]};}}'
    prefix+='w.write_all('+('&[flags as u8]' if t['flag_bytes']==1 else '&flags.to_le_bytes()')+')?;'
   else:prefix+='let _=(sizes,w);'
   prefix+='Ok(())}'
   # Flagged writers compare complete default bytes; a same-length nondefault
   # scalar must never be omitted merely because its size matches the default.
   if k!='flagged':write=f'{n}::write_prefix(['+','.join(f'self.{f["name"]}.as_slice().len()' for f in fields)+'],w)?;'+''.join(f'w.write_all(self.{f["name"]}.as_slice())?;' for f in fields)+'Ok(())'

  elif k in ['bytes','dense_vector']:
   prefix='pub fn encoded_len(sizes:impl Iterator<Item=usize>)->usize{sizes.fold(4usize,|n,size|n.saturating_add(size)'+')} '
   prefix+='#[cfg(not(target_os="none"))]pub fn write_prefix<W:std::io::Write>(sizes:impl Iterator<Item=usize>+Clone,w:&mut W)->std::io::Result<()>{let count=sizes.clone().count();'
   prefix+=f'w.write_all(&u32::try_from(count).map_err(|_|{ioerror})?.to_le_bytes())?;'
   prefix+='Ok(())}'
   write=f'{n}::write_prefix(self.items.iter().map(|v|v.as_slice().len()),w)?;for v in &self.items{{w.write_all(v.as_slice())?;}}Ok(())'
  header_size=t['flag_bytes'] if k=='flagged' else 0
  tag_bytes=t.get('tag_bytes',1) if k=='union' else 0
  tags=''.join(f'pub const TAG_{re.sub(r"(?<!^)(?=[A-Z])","_",v).upper()}:usize={i};' for i,v in enumerate(t.get('variants',[])))
  tags=tags.replace('pub const TAG_BYTES:','pub const TAG_BYTES_VARIANT:')
  entity=r"""#[derive(Clone,Debug,PartialEq,Eq)]pub struct @N@(tg::Bytes);
impl @N@{pub const FIELD_COUNT:usize=@COUNT@;pub const FIELD_NAMES:&'static[&'static str]=@FIELD_NAMES@;pub const HEADER_SIZE:usize=@HEADER_SIZE@;pub const TAG_BYTES:usize=@TAG_BYTES@;@FRAME@@TAGS@@PREFIX@}
impl Default for @N@{fn default()->Self{Self(tg::Bytes::from_static(&[@DEFAULT_BYTES@]))}}
impl Entity for @N@{type Builder=@N@Builder;type Reader<'r>=@N@Reader<'r> where Self:'r;fn new_unchecked(b:tg::Bytes)->Self{Self(b)}fn as_slice(&self)->&[u8]{&self.0}fn as_bytes(&self)->tg::Bytes{self.0.clone()}fn new_builder()->Self::Builder{Default::default()}fn as_builder(&self)->Self::Builder{@ASBUILDER@}}
#[derive(Clone,Copy,Debug)]pub struct @N@Reader<'r>(&'r[u8]);
impl<'r> Reader<'r> for @N@Reader<'r>{fn new_unchecked(b:&'r[u8])->Self{Self(b)}fn as_slice(&self)->&'r[u8]{self.0}fn verify(b:&[u8])->tg::Verify{@VERIFY@}fn span(b:&[u8])->Result<usize,VerificationError>{@SPAN@}}
impl<'r> @N@Reader<'r>{@READER@}
#[derive(Clone)]pub struct @N@Builder{@BFIELDS@}
impl Default for @N@Builder{fn default()->Self{Self{@BDEFAULT@}}}
impl @N@Builder{@BMETHODS@}
impl Builder for @N@Builder{type Entity=@N@;fn build(self)->@N@{@BUILD@@N@::new_unchecked(data.into())}#[cfg(not(target_os="none"))]fn write<W:std::io::Write>(&self,w:&mut W)->std::io::Result<()>{@WRITE@}}
"""
  if k=='flagged':
   entity=entity.replace("pub struct @N@Reader<'r>(&'r[u8]);",f"pub struct @N@Reader<'r>(&'r[u8],[u32;{len(fields)+1}]);")
   entity=entity.replace("fn new_unchecked(b:&'r[u8])->Self{Self(b)}fn as_slice", "fn new_unchecked(b:&'r[u8])->Self{Self(b,Self::__tg_spans(b).expect(\"verified flagged fields\").0)}fn as_slice")
  if k=='sequence':
   entity=entity.replace("pub struct @N@Reader<'r>(&'r[u8]);",f"pub struct @N@Reader<'r>(&'r[u8],[u32;{len(fields)+1}]);")
   entity=entity.replace("fn new_unchecked(b:&'r[u8])->Self{Self(b)}fn as_slice", "fn new_unchecked(b:&'r[u8])->Self{Self(b,Self::__tg_spans(b).expect(\"verified sequence fields\"))}fn as_slice")
  frame_constant='#[cfg(not(target_os="none"))]pub const FRAME:crate::strand_transport::Frame='+frame(n)+';' if EMIT_FRAMES else ''
  for key,value in {'DEFAULT_BYTES':','.join(str(b) for b in default_bytes(n)),'N':n,'COUNT':str(len(fields)),'FIELD_NAMES':names,'HEADER_SIZE':str(header_size),'TAG_BYTES':str(tag_bytes),'FRAME':frame_constant,'TAGS':tags,'PREFIX':prefix,'ASBUILDER':asbuilder,'VERIFY':verify,'SPAN':span,'READER':reader,'BFIELDS':bfields,'BDEFAULT':bdefault,'BMETHODS':bmethods,'BUILD':build,'WRITE':write}.items():entity=entity.replace('@'+key+'@',value)
  if k=='fixed' and t['size']==1:
   entity+=f'impl From<u8> for {n}{{fn from(v:u8)->Self{{Self::new_unchecked(tg::Bytes::copy_from_slice(&[v]))}}}}\n'
  # Byte is an inline value. Avoid the intermediate Vec<Byte> allocation and
  # second payload copy by constructing the framed byte vector in one buffer.
  if k=='bytes':
   entity+=f'impl {n}{{pub fn from_raw_data(value:&[u8])->Result<Self,VerificationError>{{let count=u32::try_from(value.len()).map_err(|_|VerificationError)?;let size=value.len().checked_add(4).ok_or(VerificationError)?;let mut data=Vec::with_capacity(size);data.extend_from_slice(&count.to_le_bytes());data.extend_from_slice(value);Ok(Self::new_unchecked(data.into()))}}}}\n'
  entity+=f'impl {n}{{pub fn from_slice(bytes:&[u8])->Result<Self,VerificationError>{{{n}Reader::verify(bytes)?;Ok(Self::new_unchecked(tg::Bytes::copy_from_slice(bytes)))}}}}\n'
  if fields:
   for f in fields:
    field, ty = f['name'], f['type']
    view = f'self.as_reader().{field}().as_slice()'
    owned = f'{rt(ty)}::new_unchecked(tg::owned_field(&self.0,{view}))' if ty!='byte' else f'{view}[0].into()'
    entity+=f'impl {n}{{pub fn {field}(&self)->{rt(ty)}{{{owned}}}}}\n'
  if k=='dense_vector':
   entity+=f'''impl IntoIterator for {n}{{type Item={child};type IntoIter=tg::DenseIter<{child}>;fn into_iter(self)->Self::IntoIter{{tg::DenseIter::new(self.0)}}}}
impl {n}{{
    pub fn len(&self)->usize{{tg::unpack_number(&self.0) as usize}}
    pub fn is_empty(&self)->bool{{self.len()==0}}
    pub fn get(&self,index:usize)->Option<{child}>{{let item=self.as_reader().get(index)?;Some({child}::new_unchecked(tg::owned_field(&self.0,item.as_slice())))}}
}}
'''
  if k=='optional':
   entity+=f'impl {n}{{pub fn to_opt(&self)->Option<{child}>{{(!self.0.is_empty()).then(||{child}::new_unchecked(self.0.clone()))}}}}\n'
  if k=='bytes':
   entity+=f'impl {n}{{pub fn raw_data(&self)->tg::Bytes{{self.0.slice(4..)}}}}\n'
  out.append(entity)
 return '\n'.join(out).rstrip()+'\n'
# C generator is defined below; descriptors and accessors share this schema.
def main():
    global SCHEMA, TS
    parser = argparse.ArgumentParser()
    parser.add_argument("--schema", type=pathlib.Path, required=True)
    parser.add_argument("--rust-out", type=pathlib.Path)
    parser.add_argument("--c-out", type=pathlib.Path)
    args = parser.parse_args()
    if not args.rust_out and not args.c_out:
        parser.error("select at least one output")
    SCHEMA = json.loads(args.schema.read_text())
    if SCHEMA.get("imports"):
        parser.error("standalone schemas must explicitly define their types")
    TS = {item["name"]: item for item in SCHEMA["types"]}
    if len(TS) != len(SCHEMA["types"]):
        parser.error("duplicate schema type")
    validate_schema()
    if args.rust_out:
        args.rust_out.write_text(rust())
    if args.c_out:
        args.c_out.write_text(c_header())


def c_header():
 names=['byte']+list(TS);ids={n:i for i,n in enumerate(names)}
 out=['''/* Generated by Stem Strand: strict schema Readers, no allocator. */
#ifndef STEM_STRAND_H
#define STEM_STRAND_H
#define STEM_STRAND_V1 1
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
typedef uint32_t tg_num_t;
typedef int tg_error;
#define TG_OK 0
#define TG_ERR 1
typedef struct {const unsigned char *ptr;uint32_t size;} tg_seg_t;
typedef struct {tg_seg_t seg;int errno;} tg_seg_res_t;
typedef struct {uint8_t kind;uint32_t size,count;const uint32_t *children;uint8_t constraint;const uint32_t *flags;} tg_type;
static uint32_t tg_unpack_number(const unsigned char*p){return (uint32_t)p[0]|((uint32_t)p[1]<<8)|((uint32_t)p[2]<<16)|((uint32_t)p[3]<<24);}
static int tg_compare(tg_seg_t a,tg_seg_t b){uint32_t n=a.size<b.size?a.size:b.size;for(uint32_t i=0;i<n;i++){if(a.ptr[i]<b.ptr[i])return -1;if(a.ptr[i]>b.ptr[i])return 1;}return a.size<b.size?-1:a.size>b.size?1:0;}
static tg_seg_t tg_part(tg_seg_t s,uint32_t start,uint32_t len){return (tg_seg_t){s.ptr+start,len};}
static const unsigned char tg_empty_vector[4]={0,0,0,0};
''']
 rows=[]
 for n in names:
  if n=='byte':rows.append('{0,1,0,0,0}');continue
  t=TS[n];k=t['kind'];fields=t.get('fields',[]);children=[ids[f['type']] for f in fields] if fields else [ids[v] for v in t.get('variants',[])]
  if 'item' in t:children=[ids[t['item']]]
  if k=='bytes':children=[ids['byte']]
  if 'allowed_values' in t:children=t['allowed_values']
  if children:out.append(f'static const uint32_t tg_children_{n}[]={{'+','.join(map(str,children))+'};')
  kind={'fixed':0,'struct':1,'sequence':3,'bytes':5,'dense_vector':6,'union':7,'optional':8,'uleb':9,'flagged':10}[k]
  size=t.get('size',0)
  if k=='union':size=t.get('tag_bytes',1)
  if k=='uleb':size=t['bits']
  if k=='flagged':size=t['flag_bytes'];out.append(f'static const uint32_t tg_flags_{n}[]={{'+','.join(str(1<<f['bit']) if 'bit' in f else '0' for f in fields)+'};')
  if k=='bytes':size=1
  if k=='dense_vector' and fixed(t['item']):size=fixed(t['item']);kind=5
  constraint={'asset_group':1,'asset_groups':2,'proof_directory':3,'shared_authorizations':5}.get(t.get('constraint'),0)
  if 'allowed_values' in t:constraint=4
  rows.append('{'+f'{kind},{size},{len(children)},'+(f'tg_children_{n}' if children else '0')+f',{constraint},'+(f'tg_flags_{n}' if k=='flagged' else '0')+'}')
 defaults={f['type'] for t in TS.values() if t['kind']=='flagged' for f in t['fields'] if 'bit' in f}
 for n in sorted(defaults):out.append(f'static const unsigned char tg_default_{n}[]={{'+','.join(str(b) for b in default_bytes(n))+'};')
 out.append('static const tg_seg_t tg_defaults[]={'+','.join('{'+f'tg_default_{n},{len(default_bytes(n))}'+'}' if n in defaults else '{0,0}' for n in names)+'};')
 out.append('static const tg_type tg_types[]={'+','.join(rows)+'};')
 for name in ['SharedCommand','CommandBody','IntentInput','OwnedAuthorization','GasAuthorization','ScriptRef','ScriptIndex']:
  if name in ids:out.append('#define TG_TYPE_'+name+' '+str(ids[name]))
 out.append(r'''
static int tg_span(uint32_t id,tg_seg_t s,uint32_t*out);
static int tg_verify(uint32_t id,tg_seg_t s);
static uint32_t tg_count(uint32_t id,tg_seg_t s){const tg_type*t=&tg_types[id];return tg_unpack_number(s.ptr);}
static tg_seg_t tg_field(uint32_t id,tg_seg_t s,uint32_t i){const tg_type*t=&tg_types[id];uint32_t pos=0,len=0;if(t->kind==10){uint32_t flags=s.ptr[0]|(t->size==2?(uint32_t)s.ptr[1]<<8:0);pos=t->size;for(uint32_t j=0;j<=i;j++){if(t->flags[j]&&!(flags&t->flags[j])){if(j==i)return tg_defaults[t->children[j]];continue;}if(!tg_span(t->children[j],tg_part(s,pos,s.size-pos),&len))return (tg_seg_t){0,0};if(j==i)return tg_part(s,pos,len);pos+=len;}return (tg_seg_t){0,0};}for(uint32_t n=0;n<=i;n++){if(!tg_span(t->children[n],tg_part(s,pos,s.size-pos),&len))return (tg_seg_t){0,0};if(n==i)return tg_part(s,pos,len);pos+=len;}return (tg_seg_t){0,0};}
static tg_seg_t tg_item(uint32_t id,tg_seg_t s,uint32_t i){const tg_type*t=&tg_types[id];if(t->kind==5)return tg_part(s,4+t->size*i,t->size);uint32_t pos=4,len=0;for(uint32_t n=0;n<=i;n++){if(!tg_span(t->children[0],tg_part(s,pos,s.size-pos),&len))return (tg_seg_t){0,0};if(n==i)return tg_part(s,pos,len);pos+=len;}return (tg_seg_t){0,0};}
static int tg_constraints(uint32_t id,tg_seg_t s);
/* Verify and find a child's end in one pass. No wire offsets or heap index. */
static int tg_parse(uint32_t id,tg_seg_t s,uint32_t*out,int verify,tg_seg_t*fields){const tg_type*t=&tg_types[id];uint32_t n=0,pos=0,len=0;
switch(t->kind){
case 0:if(s.size<t->size)return 0;pos=t->size;break;
case 1:case 3:for(uint32_t i=0;i<t->count;i++){if(!tg_parse(t->children[i],tg_part(s,pos,s.size-pos),&len,verify,0))return 0;if(fields)fields[i]=tg_part(s,pos,len);pos+=len;}break;
case 5:if(s.size<4)return 0;{uint64_t total=4+(uint64_t)tg_unpack_number(s.ptr)*t->size;if(total>s.size)return 0;pos=(uint32_t)total;}break;
case 6:if(s.size<4)return 0;n=tg_unpack_number(s.ptr);if(n>s.size-4)return 0;pos=4;for(uint32_t i=0;i<n;i++){if(!tg_parse(t->children[0],tg_part(s,pos,s.size-pos),&len,verify,0))return 0;pos+=len;}break;
case 7:if(s.size<1)return 0;n=s.ptr[0];if(n>=t->count||!tg_parse(t->children[n],tg_part(s,1,s.size-1),&len,verify,0))return 0;pos=1+len;break;
case 8:if(!s.size){pos=0;break;}if(!tg_parse(t->children[0],s,&pos,verify,0))return 0;break;
case 9:for(uint32_t i=0;i<(t->size+6)/7;i++){if(i>=s.size)return 0;uint32_t value=s.ptr[i]&127,bits=t->size-i*7;if(bits<7&&value>=(1u<<bits))return 0;if(!(s.ptr[i]&128)){if(i&&value==0)return 0;pos=i+1;break;}}if(!pos)return 0;break;
case 10:if(s.size<t->size)return 0;{uint32_t flags=s.ptr[0]|(t->size==2?(uint32_t)s.ptr[1]<<8:0),allowed=0;for(uint32_t i=0;i<t->count;i++)allowed|=t->flags[i];if(flags&~allowed)return 0;pos=t->size;for(uint32_t i=0;i<t->count;i++){if(t->flags[i]&&!(flags&t->flags[i])){if(fields)fields[i]=tg_defaults[t->children[i]];continue;}uint32_t child=t->children[i];if(t->flags[i]&&tg_types[child].kind==8)child=tg_types[child].children[0];if(!tg_parse(child,tg_part(s,pos,s.size-pos),&len,verify,0))return 0;if(verify&&t->flags[i]&&!tg_compare(tg_part(s,pos,len),tg_defaults[t->children[i]]))return 0;if(fields)fields[i]=tg_part(s,pos,len);pos+=len;}}break;
default:return 0;}
*out=pos;return !verify||!t->constraint||tg_constraints(id,tg_part(s,0,pos));}
static int tg_span(uint32_t id,tg_seg_t s,uint32_t*out){return tg_parse(id,s,out,0,0);}
static int tg_verify(uint32_t id,tg_seg_t s){uint32_t size=0;return tg_parse(id,s,&size,1,0)&&size==s.size;}
static int tg_constraints(uint32_t id,tg_seg_t s){const tg_type*t=&tg_types[id];
if(t->constraint==1){tg_seg_t entries=tg_field(id,s,1),previous={0,0};uint32_t count=tg_count(t->children[1],entries);if(!count||count>64)return 0;uint32_t pos=4,len=0,entry=tg_types[t->children[1]].children[0];for(uint32_t i=0;i<count;i++){if(!tg_span(entry,tg_part(entries,pos,entries.size-pos),&len))return 0;tg_seg_t a=tg_part(entries,pos,len);pos+=len;tg_seg_t name=tg_field(tg_types[t->children[1]].children[0],a,0),amount=tg_field(tg_types[t->children[1]].children[0],a,1);name=tg_part(name,4,name.size-4);if(name.size>32||(amount.size==1&&amount.ptr[0]==0)||(i&&tg_compare(previous,name)>=0))return 0;previous=name;}}
if(t->constraint==2){uint32_t count=tg_count(id,s),sum=0;tg_seg_t previous={0,0};if(count>64)return 0;for(uint32_t i=0;i<count;i++){tg_seg_t group=tg_item(id,s,i),policy=tg_field(t->children[0],group,0),entries=tg_field(t->children[0],group,1);if(i&&tg_compare(previous,policy)>=0)return 0;previous=policy;sum+=tg_count(tg_types[t->children[0]].children[1],entries);}if(sum>64)return 0;}
if(t->constraint==3){tg_seg_t previous={0,0};uint32_t count=tg_count(id,s),pos=4,len=0,entry=t->children[0];for(uint32_t i=0;i<count;i++){if(!tg_span(entry,tg_part(s,pos,s.size-pos),&len))return 0;tg_seg_t item=tg_part(s,pos,len),key=tg_field(entry,item,0);pos+=len;if(i&&tg_compare(previous,key)>=0)return 0;previous=key;}}
#ifdef TG_TYPE_SharedCommand
if(t->constraint==5){tg_seg_t body=tg_field(id,s,0),owned=tg_field(TG_TYPE_CommandBody,body,2),auth=tg_field(id,s,5);uint32_t owned_type=tg_types[TG_TYPE_CommandBody].children[2],auth_type=t->children[5],count=tg_count(owned_type,owned);if(count!=tg_count(auth_type,auth))return 0;tg_seg_t previous={0,0};for(uint32_t i=0;i<count;i++){tg_seg_t input=tg_item(owned_type,owned,i),point=tg_field(TG_TYPE_IntentInput,input,0);if(i){int c=tg_compare(tg_part(previous,0,32),tg_part(point,0,32));if(c>0||(c==0&&tg_unpack_number(previous.ptr+32)>=tg_unpack_number(point.ptr+32)))return 0;}previous=point;}tg_seg_t gas=tg_field(id,s,1);if(gas.size){tg_seg_t ref=tg_field(TG_TYPE_GasAuthorization,gas,0),script={0,0};uint32_t stop=count;if(ref.ptr[0]==0){uint32_t index=0;for(uint32_t i=1;i<ref.size;i++)index|=(uint32_t)(ref.ptr[i]&127)<<(7*(i-1));if(index>=count)return 0;script=tg_field(TG_TYPE_OwnedAuthorization,tg_item(auth_type,auth,index),0);stop=index;}else{script=tg_part(ref,1,ref.size-1);}for(uint32_t i=0;i<stop;i++){tg_seg_t other=tg_field(TG_TYPE_OwnedAuthorization,tg_item(auth_type,auth,i),0);if(!tg_compare(script,other))return 0;}}}
#endif
if(t->constraint==4){int found=0;for(uint32_t i=0;i<t->count;i++)if(t->children[i]==s.ptr[0])found=1;if(!found)return 0;}
return 1;}
''')
 out.append('typedef struct {tg_seg_t bytes;uint32_t item,remaining,position;} tg_cursor_t;\nstatic tg_seg_res_t tg_cursor_next(tg_cursor_t*c){uint32_t len=0;if(!c->remaining||!tg_span(c->item,tg_part(c->bytes,c->position,c->bytes.size-c->position),&len))return (tg_seg_res_t){{0,0},TG_ERR};tg_seg_t item=tg_part(c->bytes,c->position,len);c->position+=len;c->remaining--;return (tg_seg_res_t){item,TG_OK};}')
 for n,t in TS.items():
  id=ids[n];k=t['kind'];out.append(f'static int TgReader_{n}_verify(const tg_seg_t*s){{return tg_verify({id},*s)?TG_OK:TG_ERR;}}')
  if t.get('fields'):
   out.append(f'typedef struct {{tg_seg_t fields[{len(t["fields"])}];}} TgIndex_{n};')
   out.append(f'static int TgReader_{n}_index(const tg_seg_t*s,TgIndex_{n}*out){{uint32_t size=0;return tg_parse({id},*s,&size,1,out->fields)&&size==s->size?TG_OK:TG_ERR;}}')
   for i,f in enumerate(t['fields']):out.append(f'static tg_seg_t TgIndex_{n}_get_{f["name"]}(const TgIndex_{n}*index){{return index->fields[{i}];}}')
  for i,f in enumerate(t.get('fields',[])):
   if k=='struct':
    start=sum(fixed(prev['type']) for prev in t['fields'][:i]);width=fixed(f['type'])
    out.append(f'static tg_seg_t TgReader_{n}_get_{f["name"]}(const tg_seg_t*s){{return tg_part(*s,{start},{width});}}')
   else:out.append(f'static tg_seg_t TgReader_{n}_get_{f["name"]}(const tg_seg_t*s){{return tg_field({id},*s,{i});}}')
  if k in ['bytes','dense_vector']:
   out.append(f'static uint32_t TgReader_{n}_length(const tg_seg_t*s){{return tg_count({id},*s);}}')
   out.append(f'static tg_seg_res_t TgReader_{n}_get(const tg_seg_t*s,uint32_t i){{if(i>=tg_count({id},*s))return (tg_seg_res_t){{{{0,0}},TG_ERR}};return (tg_seg_res_t){{tg_item({id},*s,i),TG_OK}};}}')
   if k=='dense_vector':
    out.append(f'/* iter_verified requires a successfully verified, unchanged input. */ static void TgReader_{n}_iter_verified(const tg_seg_t*s,tg_cursor_t*out){{*out=(tg_cursor_t){{*s,{ids[t["item"]]},tg_count({id},*s),4}};}}')
    out.append(f'static int TgReader_{n}_iter(const tg_seg_t*s,tg_cursor_t*out){{if(TgReader_{n}_verify(s)!=TG_OK)return TG_ERR;TgReader_{n}_iter_verified(s,out);return TG_OK;}}')
  for f in t.get('projection',{}):
   arms=[]
   for i,v in enumerate(t['variants']):
    field=next((j for j,x in enumerate(TS[v]['fields']) if x['name']==f),None)
    tag=t.get('tag_bytes',1);expr=f'tg_field({ids[v]},tg_part(*s,{tag},s->size-{tag}),{field})' if field is not None else '(tg_seg_t){tg_empty_vector,4}'
    arms.append(f'case {i}:return {expr};')
   selector='s->ptr[0]' if t.get('tag_bytes',1)==1 else 'tg_unpack_number(s->ptr)';out.append(f'static tg_seg_t TgReader_{n}_get_{f}(const tg_seg_t*s){{switch({selector}){{'+''.join(arms)+'}return (tg_seg_t){0,0};}')
 out.append('#endif\n');return '\n'.join(out).rstrip()+'\n'
if __name__=='__main__':main()
