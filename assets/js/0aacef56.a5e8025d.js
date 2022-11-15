"use strict";(self.webpackChunkwebsite=self.webpackChunkwebsite||[]).push([[541],{3905:(e,t,n)=>{n.r(t),n.d(t,{MDXContext:()=>m,MDXProvider:()=>c,mdx:()=>f,useMDXComponents:()=>p,withMDXComponents:()=>d});var i=n(67294);function r(e,t,n){return t in e?Object.defineProperty(e,t,{value:n,enumerable:!0,configurable:!0,writable:!0}):e[t]=n,e}function a(){return a=Object.assign||function(e){for(var t=1;t<arguments.length;t++){var n=arguments[t];for(var i in n)Object.prototype.hasOwnProperty.call(n,i)&&(e[i]=n[i])}return e},a.apply(this,arguments)}function o(e,t){var n=Object.keys(e);if(Object.getOwnPropertySymbols){var i=Object.getOwnPropertySymbols(e);t&&(i=i.filter((function(t){return Object.getOwnPropertyDescriptor(e,t).enumerable}))),n.push.apply(n,i)}return n}function s(e){for(var t=1;t<arguments.length;t++){var n=null!=arguments[t]?arguments[t]:{};t%2?o(Object(n),!0).forEach((function(t){r(e,t,n[t])})):Object.getOwnPropertyDescriptors?Object.defineProperties(e,Object.getOwnPropertyDescriptors(n)):o(Object(n)).forEach((function(t){Object.defineProperty(e,t,Object.getOwnPropertyDescriptor(n,t))}))}return e}function l(e,t){if(null==e)return{};var n,i,r=function(e,t){if(null==e)return{};var n,i,r={},a=Object.keys(e);for(i=0;i<a.length;i++)n=a[i],t.indexOf(n)>=0||(r[n]=e[n]);return r}(e,t);if(Object.getOwnPropertySymbols){var a=Object.getOwnPropertySymbols(e);for(i=0;i<a.length;i++)n=a[i],t.indexOf(n)>=0||Object.prototype.propertyIsEnumerable.call(e,n)&&(r[n]=e[n])}return r}var m=i.createContext({}),d=function(e){return function(t){var n=p(t.components);return i.createElement(e,a({},t,{components:n}))}},p=function(e){var t=i.useContext(m),n=t;return e&&(n="function"==typeof e?e(t):s(s({},t),e)),n},c=function(e){var t=p(e.components);return i.createElement(m.Provider,{value:t},e.children)},u={inlineCode:"code",wrapper:function(e){var t=e.children;return i.createElement(i.Fragment,{},t)}},h=i.forwardRef((function(e,t){var n=e.components,r=e.mdxType,a=e.originalType,o=e.parentName,m=l(e,["components","mdxType","originalType","parentName"]),d=p(n),c=r,h=d["".concat(o,".").concat(c)]||d[c]||u[c]||a;return n?i.createElement(h,s(s({ref:t},m),{},{components:n})):i.createElement(h,s({ref:t},m))}));function f(e,t){var n=arguments,r=t&&t.mdxType;if("string"==typeof e||r){var a=n.length,o=new Array(a);o[0]=h;var s={};for(var l in t)hasOwnProperty.call(t,l)&&(s[l]=t[l]);s.originalType=e,s.mdxType="string"==typeof e?e:r,o[1]=s;for(var m=2;m<a;m++)o[m]=n[m];return i.createElement.apply(null,o)}return i.createElement.apply(null,n)}h.displayName="MDXCreateElement"},18987:(e,t,n)=>{n.r(t),n.d(t,{assets:()=>l,contentTitle:()=>o,default:()=>p,frontMatter:()=>a,metadata:()=>s,toc:()=>m});var i=n(83117),r=(n(67294),n(3905));const a={},o="Internal Differences from Mercurial",s={unversionedId:"internals/internal-difference-hg",id:"internals/internal-difference-hg",title:"Internal Differences from Mercurial",description:"This page assumes that you are familiar with Mercurial internals.",source:"@site/docs/internals/internal-difference-hg.md",sourceDirName:"internals",slug:"/internals/internal-difference-hg",permalink:"/docs/internals/internal-difference-hg",draft:!1,editUrl:"https://github.com/facebookexperimental/eden/tree/main/website/docs/internals/internal-difference-hg.md",tags:[],version:"current",frontMatter:{},sidebar:"tutorialSidebar",previous:{title:"Internals",permalink:"/docs/category/internals"}},l={},m=[{value:"Visibility",id:"visibility",level:2},{value:"Phase",id:"phase",level:2},{value:"Obsolescence",id:"obsolescence",level:2},{value:"Storage Format",id:"storage-format",level:2},{value:"Protocols",id:"protocols",level:2},{value:"Python 3 and Unicode",id:"python-3-and-unicode",level:2},{value:"Pure Python Support",id:"pure-python-support",level:2},{value:"Git Support",id:"git-support",level:2}],d={toc:m};function p(e){let{components:t,...n}=e;return(0,r.mdx)("wrapper",(0,i.Z)({},d,n,{components:t,mdxType:"MDXLayout"}),(0,r.mdx)("h1",{id:"internal-differences-from-mercurial"},"Internal Differences from Mercurial"),(0,r.mdx)("admonition",{type:"note"},(0,r.mdx)("p",{parentName:"admonition"},"This page assumes that you are familiar with Mercurial internals.")),(0,r.mdx)("h2",{id:"visibility"},"Visibility"),(0,r.mdx)("p",null,"Mercurial treats all commits as visible by default, using obsolescence data to\nmark obsoleted commits as invisible."),(0,r.mdx)("p",null,'Sapling treats all commits as invisible by default, using "visible heads"\nand bookmark references to mark commits and their ancestors as visible. This\nis similar to Git.'),(0,r.mdx)("p",null,"Performance wise, too much obsolescence data can slow down a Mercurial repo.\nSimilarly, too many bookmarks and visible heads can slow down a Sapling repo.\nHowever, obsolescence data can grow over time unbounded while bookmarks and\nvisible heads can shrink using commands like ",(0,r.mdx)("inlineCode",{parentName:"p"},"sl bookmark -d")," and ",(0,r.mdx)("inlineCode",{parentName:"p"},"sl hide"),".\nPractically, we assume a bounded number of bookmarks and visible heads."),(0,r.mdx)("p",null,'Mercurial has a "repo view" layer to forbid access to hidden commits.\nAccessing them (for example, using the ',(0,r.mdx)("inlineCode",{parentName:"p"},"predecessors()")," revset) requires a\nglobal flag ",(0,r.mdx)("inlineCode",{parentName:"p"},"--hidden"),'. Sapling removes the "repo view" layer. Revsets like\n',(0,r.mdx)("inlineCode",{parentName:"p"},"all()"),", ",(0,r.mdx)("inlineCode",{parentName:"p"},"children()"),", ",(0,r.mdx)("inlineCode",{parentName:"p"},"descendants()")," handle the visibility transparently by\nnot including invisible commits. Revsets like ",(0,r.mdx)("inlineCode",{parentName:"p"},"predecessors()")," do not care\nabout visibility and return invisible commits.  If the user explicitly requests\nthem using commit hashes, they will be included."),(0,r.mdx)("h2",{id:"phase"},"Phase"),(0,r.mdx)("p",null,'Mercurial tracks phases (public, draft, secret) explicitly using "phase roots".'),(0,r.mdx)("p",null,"Sapling infers phases from remote bookmarks and visibility. Remote bookmarks\nand their ancestors are considered public. Other visible commits are draft.\nInvisible commits are secret."),(0,r.mdx)("h2",{id:"obsolescence"},"Obsolescence"),(0,r.mdx)("p",null,'Mercurial uses the "obsstore" to track commit rewrites. Sapling uses\n"mutation". Their differences are:'),(0,r.mdx)("ul",null,(0,r.mdx)("li",{parentName:"ul"},"Obsstore decides visibility. Mutation does not decide visibility."),(0,r.mdx)("li",{parentName:"ul"},'Obsstore supports "prune" operation to remove a commit without a successor\ncommit. Mutation requires at least one successor commit so it cannot track\n"prune" rewrites.'),(0,r.mdx)("li",{parentName:"ul"},"If all successors of a mutation are invisible, then the mutation is ignored.\nThis means mutation can be implicitly tracked by visibility. Restoring\nvisibility to a previous state during an undo operation effectively\nrestores the commit rewrite state.")),(0,r.mdx)("p",null,"Implementation wise, mutation uses IndexedLog for ",(0,r.mdx)("inlineCode",{parentName:"p"},"O(log N)")," lookup. Nothing in\nSapling requires ",(0,r.mdx)("inlineCode",{parentName:"p"},"O(N)")," loading of the entire mutation data."),(0,r.mdx)("h2",{id:"storage-format"},"Storage Format"),(0,r.mdx)("p",null,"Mercurial uses ",(0,r.mdx)("a",{parentName:"p",href:"https://www.mercurial-scm.org/wiki/Revlog"},"Revlog")," as its main\nfile format. Sapling uses IndexedLog instead."),(0,r.mdx)("p",null,"For working copy state, Mercurial uses ",(0,r.mdx)("a",{parentName:"p",href:"https://www.mercurial-scm.org/wiki/DirState"},"Dirstate"),".\nSapling switched to TreeState in 2017. Mercurial 5.9 released in 2021\nintroduced ",(0,r.mdx)("a",{parentName:"p",href:"https://www.mercurial-scm.org/repo/hg/file/tip/mercurial/helptext/internals/dirstate-v2.txt"},"Dirstate v2"),"\nthat improves performance in a similar way."),(0,r.mdx)("p",null,"For repo references such as bookmarks and remote bookmarks, Mercurial tracks\nthem in individual files like ",(0,r.mdx)("inlineCode",{parentName:"p"},".hg/bookmarks"),". Sapling uses MetaLog\nto track them so changes are across state files are atomic."),(0,r.mdx)("h2",{id:"protocols"},"Protocols"),(0,r.mdx)("p",null,"Mercurial supports ssh and http wireprotocols. Sapling's main protocol is\ndefined in a Rust ",(0,r.mdx)("inlineCode",{parentName:"p"},"EdenApi")," trait. It is very different from the original\nwireprotocols."),(0,r.mdx)("p",null,"There are two implementations of the ",(0,r.mdx)("inlineCode",{parentName:"p"},"EdenApi")," trait: an HTTP client that talks\nto a supported server and an ",(0,r.mdx)("inlineCode",{parentName:"p"},"EagerRepo")," for lightweight local testing. The\nHTTP implementation uses multiple connections to saturate network bandwidth\nfor better performance."),(0,r.mdx)("h2",{id:"python-3-and-unicode"},"Python 3 and Unicode"),(0,r.mdx)("p",null,"Python 3 switched the ",(0,r.mdx)("inlineCode",{parentName:"p"},"str")," type from ",(0,r.mdx)("inlineCode",{parentName:"p"},"bytes")," to ",(0,r.mdx)("inlineCode",{parentName:"p"},"unicode"),". This affects\nkeyword arguments, and stdlib APIs like ",(0,r.mdx)("inlineCode",{parentName:"p"},"os.listdir"),", ",(0,r.mdx)("inlineCode",{parentName:"p"},"sys.argv"),"."),(0,r.mdx)("p",null,"Sapling adopts Unicode more aggressively. Command line arguments, bookmark\nnames, file names, config files are considered Unicode and are encoded using\nutf-8 during serialization. Sapling does not turn Python keyword arguments and\nstdlib output back to bytes."),(0,r.mdx)("p",null,"Treating file names as utf-8 allows Sapling to read and write correct file\nnames between Windows and ","*","nix systems for a given repo."),(0,r.mdx)("h2",{id:"pure-python-support"},"Pure Python Support"),(0,r.mdx)("p",null,"Mercurial maintains a pure Python implementation. It can run without building\nwith a C or Rust compiler by setting ",(0,r.mdx)("inlineCode",{parentName:"p"},"HGMODULEPOLICY")," to ",(0,r.mdx)("inlineCode",{parentName:"p"},"py"),". This is not\npossible for Sapling."),(0,r.mdx)("h2",{id:"git-support"},"Git Support"),(0,r.mdx)("p",null,"There are 2 extensions that add Git support to Mercurial:"),(0,r.mdx)("ul",null,(0,r.mdx)("li",{parentName:"ul"},(0,r.mdx)("a",{parentName:"li",href:"https://www.mercurial-scm.org/wiki/HgGit"},"hg-git")),(0,r.mdx)("li",{parentName:"ul"},(0,r.mdx)("a",{parentName:"li",href:"https://www.mercurial-scm.org/repo/hg/file/tip/hgext/git/__init__.py"},"hgext/git"))),(0,r.mdx)("p",null,(0,r.mdx)("inlineCode",{parentName:"p"},"hg-git")," mirrors the bare Git repo to a regular hg repo. Therefore\nit double stores file content, and produces different hashes."),(0,r.mdx)("p",null,(0,r.mdx)("inlineCode",{parentName:"p"},"hgext/git")," tries to be compatible with an existing Git repo. Therefore\nit is limited to git specifications like what the ",(0,r.mdx)("inlineCode",{parentName:"p"},".git")," directory should\ncontain and in what format."),(0,r.mdx)("p",null,"Sapling treats Git as an implementation of its repo data abstraction.\nThis means:"),(0,r.mdx)("ul",null,(0,r.mdx)("li",{parentName:"ul"},"The working copy implementation is Sapling's. It can integrate with our\nvirtualized working copy filesystem in the future."),(0,r.mdx)("li",{parentName:"ul"},"The repo data implementation can adopt Sapling's components in the future for\nbenefits like on-demand fetching, data bookkeeping without repack."),(0,r.mdx)("li",{parentName:"ul"},"Git commands are not supported in Sapling's Git repo.")))}p.isMDXComponent=!0}}]);