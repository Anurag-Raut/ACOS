import subprocess,sys,os,re,collections
V=sys.argv[1]; SR=sys.argv[2]; files=sys.argv[3:]
def syms(p,defined):
    out=subprocess.run(['nm','-D','--defined-only' if defined else '--undefined-only',p],capture_output=True,text=True).stdout
    s=set()
    for l in out.splitlines():
        parts=l.split()
        if not parts: continue
        n=parts[-1].split('@')[0]
        s.add(n)
    return s
def needed(p):
    out=subprocess.run(['readelf','-d',p],capture_output=True,text=True).stdout
    return re.findall(r'NEEDED.*\[(.*)\]',out)
dirs=[SR+'/system/apex/com.android.runtime/lib64/bionic',SR+'/system/lib64',V,V+'/egl',V+'/hw']
def find(n):
    for d in dirs:
        q=os.path.join(d,n)
        if os.path.exists(q): return q
    return None
for f in files:
    p=os.path.join(V,f)
    und=syms(p,False)
    # weak undefined ignore: nm marks 'w'
    nd=needed(p); defs=set(); missing=[]
    seen=set()
    stack=list(nd)
    while stack:
        n=stack.pop()
        if n in seen: continue
        seen.add(n)
        q=find(n)
        if not q: missing.append(n); continue
        defs|=syms(q,True); stack+=needed(q)
    un=sorted(x for x in und if x not in defs)
    print(f"== {f}: NEEDED missing in guest={missing}; unresolved undefined syms={len(un)}")
    for x in un[:40]: print("   ",x)
