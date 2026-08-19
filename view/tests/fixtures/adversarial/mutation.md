# Mutation and namespace confusion

<a id="body">clobber body</a>

<a id="querySelector">clobber method</a>

<a id="para">ordinary id</a>

<a name="documentElement">clobber name</a>

<p tabindex="0" contenteditable="true" accesskey="x" draggable="true">focus bait</p>

<table><tr><td background="//evil.example.com/x" bgcolor="red">cell</td></tr></table>

<a href="https://ok.example/x" download="x" ping="//evil.example.com/p">download</a>

<a href="#frag" target="_blank" rel="opener">fragment in a new tab</a>

<noscript><p title="</noscript><img src=x onerror=window.__pwned=1>"></noscript>

<template><img src=x onerror=window.__pwned=1></template>

<svg><title><img src=x onerror=window.__pwned=1></title></svg>

<svg><rect/><p onclick="window.__pwned=1">hoisted out of svg</p></svg>

<![CDATA[<script>window.__pwned=1</script>]]>

<!--><script>window.__pwned=1</script>-->

<!--[if IE]><script>window.__pwned=1</script><![endif]-->

<svg></p><style><a id="</style><img src=1 onerror=window.__pwned=1>"></style></svg>

<math><mtext><table><mglyph><style><!--</style><img title="--><img src=x onerror=window.__pwned=1>"></mglyph></table></mtext></math>

<form><math><mtext></form><form><mglyph><style></math><img src onerror=window.__pwned=1></style></mglyph></form>
