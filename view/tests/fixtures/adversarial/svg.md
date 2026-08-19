# SVG

<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" class="diagram" id="fig-1" role="img">
  <title>Allowed title</title>
  <desc>Allowed description</desc>
  <g transform="translate(2,2)" fill="none" stroke="currentColor" stroke-width="2">
    <path d="M1 1 L10 10"/>
    <circle cx="5" cy="5" r="4"/>
    <rect x="1" y="1" width="8" height="8" rx="1"/>
    <line x1="0" y1="0" x2="9" y2="9"/>
    <polyline points="0,0 5,5"/>
    <polygon points="0,0 5,0 5,5"/>
    <ellipse cx="4" cy="4" rx="3" ry="2"/>
  </g>
  <script>window.__pwned = true</script>
  <foreignObject><body xmlns="http://www.w3.org/1999/xhtml"><script>window.__pwned=1</script></body></foreignObject>
  <animate attributeName="x" onbegin="window.__pwned=true"/>
  <set attributeName="href" to="javascript:window.__pwned=true"/>
  <use href="https://evil.example.com/x.svg#icon"/>
  <use href="#fig-1"/>
  <image href="https://evil.example.com/tracker.png"/>
  <a href="javascript:window.__pwned=true"><text>svg anchor</text></a>
  <style>* { fill: url(javascript:alert(1)); }</style>
</svg>
