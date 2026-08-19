# Embeds

<iframe src="https://evil.example.com" srcdoc="<script>window.__pwned=1</script>"></iframe>
<frameset><frame src="https://evil.example.com"></frameset>
<object data="https://evil.example.com/x.swf"></object>
<embed src="https://evil.example.com/x.swf">
<applet code="Evil.class"></applet>
<form action="https://evil.example.com" method="post">
  <input name="secret" value="x">
  <button formaction="https://evil.example.com">Send</button>
  <select><option>one</option></select>
  <textarea>hi</textarea>
</form>
<audio src="https://evil.example.com/a.mp3" autoplay></audio>
<video src="https://evil.example.com/v.mp4" autoplay><source src="https://evil.example.com/v.webm"></video>
<canvas id="c" width="10" height="10"></canvas>
<base href="https://evil.example.com/">
<meta http-equiv="refresh" content="0;url=https://evil.example.com">
<link rel="stylesheet" href="https://evil.example.com/x.css">
<template><script>window.__pwned=1</script></template>
<noscript><iframe src="https://evil.example.com"></iframe></noscript>
<math><mtext><script>window.__pwned=1</script></mtext></math>
