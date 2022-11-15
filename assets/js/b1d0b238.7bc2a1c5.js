"use strict";(self.webpackChunkwebsite=self.webpackChunkwebsite||[]).push([[3334],{3905:(e,t,r)=>{r.r(t),r.d(t,{MDXContext:()=>c,MDXProvider:()=>m,mdx:()=>h,useMDXComponents:()=>p,withMDXComponents:()=>u});var n=r(67294);function o(e,t,r){return t in e?Object.defineProperty(e,t,{value:r,enumerable:!0,configurable:!0,writable:!0}):e[t]=r,e}function a(){return a=Object.assign||function(e){for(var t=1;t<arguments.length;t++){var r=arguments[t];for(var n in r)Object.prototype.hasOwnProperty.call(r,n)&&(e[n]=r[n])}return e},a.apply(this,arguments)}function i(e,t){var r=Object.keys(e);if(Object.getOwnPropertySymbols){var n=Object.getOwnPropertySymbols(e);t&&(n=n.filter((function(t){return Object.getOwnPropertyDescriptor(e,t).enumerable}))),r.push.apply(r,n)}return r}function l(e){for(var t=1;t<arguments.length;t++){var r=null!=arguments[t]?arguments[t]:{};t%2?i(Object(r),!0).forEach((function(t){o(e,t,r[t])})):Object.getOwnPropertyDescriptors?Object.defineProperties(e,Object.getOwnPropertyDescriptors(r)):i(Object(r)).forEach((function(t){Object.defineProperty(e,t,Object.getOwnPropertyDescriptor(r,t))}))}return e}function s(e,t){if(null==e)return{};var r,n,o=function(e,t){if(null==e)return{};var r,n,o={},a=Object.keys(e);for(n=0;n<a.length;n++)r=a[n],t.indexOf(r)>=0||(o[r]=e[r]);return o}(e,t);if(Object.getOwnPropertySymbols){var a=Object.getOwnPropertySymbols(e);for(n=0;n<a.length;n++)r=a[n],t.indexOf(r)>=0||Object.prototype.propertyIsEnumerable.call(e,r)&&(o[r]=e[r])}return o}var c=n.createContext({}),u=function(e){return function(t){var r=p(t.components);return n.createElement(e,a({},t,{components:r}))}},p=function(e){var t=n.useContext(c),r=t;return e&&(r="function"==typeof e?e(t):l(l({},t),e)),r},m=function(e){var t=p(e.components);return n.createElement(c.Provider,{value:t},e.children)},d={inlineCode:"code",wrapper:function(e){var t=e.children;return n.createElement(n.Fragment,{},t)}},f=n.forwardRef((function(e,t){var r=e.components,o=e.mdxType,a=e.originalType,i=e.parentName,c=s(e,["components","mdxType","originalType","parentName"]),u=p(r),m=o,f=u["".concat(i,".").concat(m)]||u[m]||d[m]||a;return r?n.createElement(f,l(l({ref:t},c),{},{components:r})):n.createElement(f,l({ref:t},c))}));function h(e,t){var r=arguments,o=t&&t.mdxType;if("string"==typeof e||o){var a=r.length,i=new Array(a);i[0]=f;var l={};for(var s in t)hasOwnProperty.call(t,s)&&(l[s]=t[s]);l.originalType=e,l.mdxType="string"==typeof e?e:o,i[1]=l;for(var c=2;c<a;c++)i[c]=r[c];return n.createElement.apply(null,i)}return n.createElement.apply(null,r)}f.displayName="MDXCreateElement"},78198:(e,t,r)=>{r.r(t),r.d(t,{assets:()=>s,contentTitle:()=>i,default:()=>p,frontMatter:()=>a,metadata:()=>l,toc:()=>c});var n=r(83117),o=(r(67294),r(3905));const a={sidebar_position:10},i="Overview",l={unversionedId:"scale/overview",id:"scale/overview",title:"Overview",description:"Sapling supports large monorepos that have tens of millions of files and",source:"@site/docs/scale/overview.md",sourceDirName:"scale",slug:"/scale/overview",permalink:"/docs/scale/overview",draft:!1,editUrl:"https://github.com/facebookexperimental/eden/tree/main/website/docs/scale/overview.md",tags:[],version:"current",sidebarPosition:10,frontMatter:{sidebar_position:10},sidebar:"tutorialSidebar",previous:{title:"Working at Scale",permalink:"/docs/category/working-at-scale"},next:{title:"Axes of Scale",permalink:"/docs/scale/axes"}},s={},c=[{value:"Performance Challenges",id:"performance-challenges",level:2},{value:"Other Challenges",id:"other-challenges",level:2},{value:"Note about &quot;Distributed&quot;",id:"note-about-distributed",level:2}],u={toc:c};function p(e){let{components:t,...r}=e;return(0,o.mdx)("wrapper",(0,n.Z)({},u,r,{components:t,mdxType:"MDXLayout"}),(0,o.mdx)("h1",{id:"overview"},"Overview"),(0,o.mdx)("p",null,"Sapling supports large monorepos that have tens of millions of files and\ncommits, with tens of thousands of contributors."),(0,o.mdx)("h2",{id:"performance-challenges"},"Performance Challenges"),(0,o.mdx)("p",null,"This scale imposes performance challenges in various areas. Operations that\nrequire all files or all commits (O(files) or O(commits)) space or time\ncomplexities are gradually no longer affordable. Push throughput could also be\nan issue."),(0,o.mdx)("p",null,"Over time, Sapling made many improvements to tackle the above challenges:"),(0,o.mdx)("ul",null,(0,o.mdx)("li",{parentName:"ul"},"On-demand historical file fetching (remotefilelog, 2013)"),(0,o.mdx)("li",{parentName:"ul"},"File system monitor for faster working copy status (watchman, 2014)"),(0,o.mdx)("li",{parentName:"ul"},"In-repo sparse profile to shrink working copy (2015)"),(0,o.mdx)("li",{parentName:"ul"},"Limit references to exchange (selective pull, 2016)"),(0,o.mdx)("li",{parentName:"ul"},"On-demand historical tree fetching (2017)"),(0,o.mdx)("li",{parentName:"ul"},"Incremental updates to working copy state (treestate, 2017)"),(0,o.mdx)("li",{parentName:"ul"},"New server infrastructure for push throughput and faster indexes (Mononoke, 2017)"),(0,o.mdx)("li",{parentName:"ul"},"Virtualized working copy for on-demand currently checked out file or tree fetching (EdenFS, 2018)"),(0,o.mdx)("li",{parentName:"ul"},"Faster commit graph algorithms (segmented changelog, 2020)"),(0,o.mdx)("li",{parentName:"ul"},"On-demand commit fetching (2021)")),(0,o.mdx)("h2",{id:"other-challenges"},"Other Challenges"),(0,o.mdx)("p",null,'Besides improving scale and performance, we also strove to build a robust\ndevelopment experience.  To avoid developers losing their work due to hardware\nfailures, we back up all commits as they are created to our "commit cloud".\nUnlike other systems, the developer doesn\'t have to expressly push their\ncommits for them to be shareable and durable.'),(0,o.mdx)("h2",{id:"note-about-distributed"},'Note about "Distributed"'),(0,o.mdx)("p",null,"Sapling started from Mercurial as a distributed source control system, it then\ntransitioned to a client-server architecture to solve the challenges.\nThe server can utilize distributed storage and pre-build various kinds of\nindexes to provide more efficient operations."),(0,o.mdx)("p",null,'While Sapling is less "distributed", we tried to make the difference\ntransparent to the user. For example, our lazy commit graph implementation does\nnot require extra commands to "deepen" the graph. The user sees full history.'),(0,o.mdx)("p",null,"That said, we do drop support of pulling from a lazy repo. Commit cloud covers\nthese use-cases."))}p.isMDXComponent=!0}}]);