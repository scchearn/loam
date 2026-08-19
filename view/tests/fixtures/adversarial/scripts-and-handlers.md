# Handlers and scripts

<script>window.__pwned = true;</script>
<script type="module">import('data:text/javascript,window.__pwned=1')</script>

<p onclick="window.__pwned = true">Click bait</p>
<div onmouseover="window.__pwned = true" ONLOAD="window.__pwned = true">Hover bait</div>
<img src="x" onerror="window.__pwned = true" alt="broken">
<a href="#top" onfocus="window.__pwned = true">Anchor with handler</a>
<svg><circle cx="5" cy="5" r="4" onload="window.__pwned = true"/></svg>

<style>body { background: url("javascript:alert(1)"); }</style>
<p style="background-image: url(javascript:alert(1))">Styled</p>
