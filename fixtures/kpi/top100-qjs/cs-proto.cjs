try {
  const C = require('./node_modules/combined-stream/lib/combined_stream.js');
  console.log('ctor', typeof C);
  console.log('emit', typeof C.prototype.emit, 'on', typeof C.prototype.on, 'pipe', typeof C.prototype.pipe);
  const c = new C();
  console.log('inst', typeof c.emit, typeof c.on, typeof c.pipe);
} catch(e){
  console.log(e && e.stack ? e.stack : String(e));
}
