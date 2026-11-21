import argparse, ast, hashlib, json, pathlib, re, collections, subprocess
ROOTS=['apps','crates','frontends','.config','tests']
EXCLUDE={'vendor','third_party','fixtures','testdata','test-data','snapshots','golden'}
EXT={'.rs','.py','.js','.jsx','.mjs','.cjs','.ts','.tsx','.sql','.slt','.css'}

def sha(s): return hashlib.sha256(s.encode()).hexdigest()
def masked_rust(src):
    out=list(src); n=len(src); i=0
    def blank(a,b):
        for k in range(a,b):
            if out[k]!='\n': out[k]=' '
    while i<n:
        if src.startswith('//',i):
            j=src.find('\n',i); j=n if j<0 else j; blank(i,j); i=j; continue
        if src.startswith('/*',i):
            j=i+2; depth=1
            while j<n and depth:
                if src.startswith('/*',j): depth+=1; j+=2
                elif src.startswith('*/',j): depth-=1; j+=2
                else: j+=1
            blank(i,j); i=j; continue
        # raw strings, including byte raw strings
        m=re.compile(r'(?:br|r)(#+)"').match(src,i)
        if m:
            opener=m.group(0); hashes=m.group(1); j=i+len(opener); end='"'+hashes; k=src.find(end,j); k=n if k<0 else k+len(end); blank(i,k); i=k; continue
        if src[i]=='b' and i+1<n and src[i+1]=='"': start=i+1
        else: start=i
        if src[start]=='"':
            j=start+1
            while j<n:
                if src[j]=='\\': j+=2; continue
                if src[j]=='"': j+=1; break
                j+=1
            blank(i,j); i=j; continue
        if src[i]=="'":
            # Character literals have a closing quote; lifetimes do not.
            j=i+1; esc=False; found=False
            while j<min(n,i+12) and src[j]!='\n':
                if not esc and src[j]=="'": found=True; j+=1; break
                if not esc and src[j]=='\\': esc=True
                else: esc=False
                j+=1
            if found: blank(i,j); i=j; continue
        i+=1
    return ''.join(out)

def rust_rows(path, rel, src):
    code=masked_rust(src); rows=[]; seen=set()
    attrs=list(re.finditer(r'#\s*\[\s*([A-Za-z_][\w:]*)\s*(?:\([^\]]*\))?\s*\]',code))
    testattr=re.compile(r'^(?:test|tokio::test|async_std::test|actix_rt::test|rstest|test_case|case|proptest|quickcheck)$')
    for a in attrs:
        tag=a.group(1)
        if not testattr.match(tag): continue
        f=re.search(r'\b(?:async\s+)?fn\s+([A-Za-z_][A-Za-z_0-9]*)\b',code[a.end():a.end()+2500])
        if not f: continue
        fs=a.end()+f.start(); name=f.group(1); start=a.start()
        key=(name,fs)
        if key in seen: continue
        seen.add(key)
        # include preceding adjacent test attrs/annotations, but start hash at anchor
        signature_end=a.end()+f.end(); brace=code.find('{',signature_end); end=brace+1 if brace>=0 else signature_end; depth=1 if brace>=0 else 0
        while depth and end<len(code):
            if code[end]=='{': depth+=1
            elif code[end]=='}': depth-=1
            end+=1
        body=code[brace:end] if brace>=0 else ''
        nearby=code[max(0,start-180):fs]
        ignored=bool(re.search(r'#\s*\[\s*ignore(?:\s*=|\s*\])',nearby))
        family=tag
        testkind='in_process'
        if re.search(r'(?:std|tokio)::process::Command|Command\s*::\s*new|\.spawn\s*\(',body): testkind='process_child'
        if 'tests/keel/' in rel or '/keel/' in rel: testkind='process_harness'
        line=src.count('\n',0,start)+1
        rows.append(dict(language='rust',path=rel,name=name,line=line,end_line=src.count('\n',0,end)+1,source_sha256=sha(src[start:end]),scope='paused' if ignored else 'not_explicitly_ignored',test_kind=testkind,uncertainty='function/body detected after comment-string masking; cfg feature reachability not evaluated',family=family))
    # Macro families that expand into tests but do not expose individual test functions.
    for m in re.finditer(r'\b(proptest|quickcheck|rstest|test_case)\s*!\s*\{',code):
        line=src.count('\n',0,m.start())+1
        # brace span
        b=code.find('{',m.start()); d=1; e=b+1
        while d and e<len(code):
            d += (code[e]=='{')-(code[e]=='}'); e+=1
        rows.append(dict(language='rust',path=rel,name=f'{m.group(1)}!@{line}',line=line,end_line=src.count('\n',0,e)+1,source_sha256=sha(src[m.start():e]),scope='unknown',test_kind='macro_generated',uncertainty='macro expansion and case cardinality not evaluated',family=m.group(1)))
    return rows

def is_test_path(p):
    parts=p.parts
    return any(x in {'tests','test','__tests__'} for x in parts) or re.search(r'(^|[._-])(test|spec)([._-]|$)',p.name,re.I) is not None

def python_rows(rel,src):
    try: tree=ast.parse(src)
    except Exception: return []
    lines=src.splitlines(keepends=True); offsets=[0]
    for l in lines: offsets.append(offsets[-1]+len(l))
    rows=[]
    def walk(node,cls=''):
      for x in ast.iter_child_nodes(node):
        if isinstance(x,(ast.FunctionDef,ast.AsyncFunctionDef)):
          dec=ast.get_source_segment(src,x) or ''
          test=x.name.startswith('test') or (cls and x.name.startswith('test'))
          if not test: walk(x,cls); continue
          start=min([x.lineno]+[d.lineno for d in x.decorator_list]); end=x.end_lineno or x.lineno
          txt=src[offsets[start-1]:offsets[end]]
          marks=[ast.unparse(d) for d in x.decorator_list]
          paused=any('skip' in d for d in marks)
          params=[d for d in marks if 'parametrize' in d]
          body=txt
          kind='process_child' if re.search(r'\b(subprocess\.(?:run|Popen|call)|Popen\s*\(|run\s*\(|pexpect\.)',body) else 'in_process'
          rows.append(dict(language='python',path=rel,name=x.name,line=start,end_line=end,source_sha256=sha(txt),scope='paused' if paused else 'not_explicitly_ignored',test_kind=kind,uncertainty='Python AST function discovery; dynamic collection/runtime fixtures not executed',family='pytest' if is_test_path(pathlib.PurePosixPath(rel)) else 'unittest'))
          if params: rows.append(dict(language='python',path=rel,name=x.name+'::parametrize',line=start,end_line=end,source_sha256=sha('\n'.join(params)),scope='unknown',test_kind='parameterized_family',uncertainty='parameter cases are an additional obligation family; exact expansion not evaluated',family='pytest.mark.parametrize'))
          walk(x,cls)
        elif isinstance(x,ast.ClassDef): walk(x,x.name)
        else: walk(x,cls)
    walk(tree)
    return rows

def js_mask_comments(src):
    out=list(src); n=len(src); i=0
    def blank(a,b):
      for k in range(a,b):
       if out[k]!='\n': out[k]=' '
    while i<n:
      if src.startswith('//',i):
       j=src.find('\n',i); j=n if j<0 else j; blank(i,j); i=j; continue
      if src.startswith('/*',i):
       j=src.find('*/',i+2); j=n if j<0 else j+2; blank(i,j); i=j; continue
      if src[i] in ('"',"'",'`'):
       q=src[i]; j=i+1
       while j<n:
        if src[j]=='\\': j+=2; continue
        if src[j]==q: j+=1; break
        j+=1
       # preserve string contents so names can be extracted; mask comments only
       i=j; continue
      i+=1
    return ''.join(out)

def js_rows(rel,src):
    if not is_test_path(pathlib.PurePosixPath(rel)): return []
    code=js_mask_comments(src); rows=[]
    pat=re.compile(r'\b(?:it|test|specify)\s*(?:\.\s*(?:only|skip|each|concurrent|todo)\s*)?\(\s*([\'"`])((?:\\.|(?!\1).)*?)\1',re.S)
    for m in pat.finditer(code):
      title=m.group(2).strip(); line=src.count('\n',0,m.start())+1
      dynamic=bool('${' in title or re.search(r'\.\s*each\s*',code[max(0,m.start()-20):m.start()]))
      paused=bool(re.search(r'\b(?:it|test|specify)\s*\.\s*(?:skip|todo)\s*\(',code[max(0,m.start()-20):m.start()+15]))
      end=m.end(); rows.append(dict(language='javascript' if pathlib.Path(rel).suffix in ('.js','.jsx','.mjs','.cjs') else 'typescript',path=rel,name=title or f'<dynamic>@{line}',line=line,end_line=src.count('\n',0,end)+1,source_sha256=sha(src[m.start():end]),scope='paused' if paused else ('unknown' if dynamic else 'not_explicitly_ignored'),test_kind='test_declaration',uncertainty='static declaration name found; callbacks/parameterized expansion not executed',family='test/it/specify'))
    return rows

def scan(root):
    rows=[]; assets=[]
    for base in ROOTS:
      d=root/base
      if not d.exists(): continue
      for p in d.rglob('*'):
       if not p.is_file() or p.suffix.lower() not in EXT: continue
       rel=p.relative_to(root).as_posix(); parts=set(p.parts)
       if parts & EXCLUDE: continue
       if rel.startswith(('tests/keel/evidence/','tests/keel/fixtures/')): continue
       try: src=p.read_text(errors='replace')
       except Exception: continue
       if p.suffix=='.rs':
         if re.search(r'#\s*\[\s*(?:test|tokio::test|async_std::test|actix_rt::test|rstest|test_case|case|proptest|quickcheck)\b|\b(?:proptest|quickcheck|rstest|test_case)\s*!',src): rows.extend(rust_rows(p,rel,src))
       elif p.suffix=='.py' and is_test_path(p.relative_to(root)): rows.extend(python_rows(rel,src))
       elif p.suffix in ('.js','.jsx','.mjs','.cjs','.ts','.tsx') and is_test_path(p.relative_to(root)): rows.extend(js_rows(rel,src))
       elif p.suffix in ('.sql','.slt','.css') and (is_test_path(p.relative_to(root)) or rel.startswith(('frontends/','tests/'))):
         # Test data is tracked separately: a fixture is not itself proof of an assertion.
         assets.append(dict(language={'.sql':'sql','.slt':'sql','.css':'css'}[p.suffix],path=rel,source_sha256=sha(src),classification='test_asset_candidate',uncertainty='file-level fixture/query corpus; assertion/runner linkage must be reviewed'))
    return rows,assets

def add_stable_ids(rows):
    rows.sort(key=lambda row: (row['language'], row['path'], row['name'], row['line']))
    counts = collections.Counter()
    for row in rows:
        key = (row['language'], row['path'], row['name'])
        counts[key] += 1
        ordinal = counts[key]
        row['same_name_ordinal'] = ordinal
        row['id'] = f"{row['language']}:{row['path']}::{row['name']}#{ordinal}"
    return rows

def main():
    parser = argparse.ArgumentParser(description='Extract source-level test obligations; this is an inventory utility, not a test runner.')
    parser.add_argument('--root', required=True, type=pathlib.Path, help='checked-out source tree to scan')
    parser.add_argument('--revision', required=True, help='revision label to record, normally the checked-out Git commit')
    parser.add_argument('--output', required=True, type=pathlib.Path, help='JSON output path')
    args = parser.parse_args()
    root = args.root.resolve()
    actual_revision = subprocess.check_output(
        ['git', '-C', str(root), 'rev-parse', 'HEAD'], text=True
    ).strip()
    requested_revision = subprocess.check_output(
        ['git', '-C', str(root), 'rev-parse', args.revision], text=True
    ).strip()
    if actual_revision != requested_revision:
        parser.error(f'--root is at {actual_revision}, but --revision resolves to {requested_revision}')
    rows, assets = scan(root)
    rows = add_stable_ids(rows)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps({'root_revision': actual_revision, 'rows': rows, 'test_assets': assets}, indent=2, sort_keys=True) + '\n')
    print(json.dumps({'root_revision': actual_revision, 'rows': len(rows), 'assets': len(assets), 'by_language': dict(collections.Counter(r['language'] for r in rows)), 'by_kind': dict(collections.Counter(r['test_kind'] for r in rows)), 'by_scope': dict(collections.Counter(r['scope'] for r in rows))}, sort_keys=True))

if __name__ == '__main__':
    main()
