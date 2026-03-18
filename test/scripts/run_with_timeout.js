var args = WScript.Arguments;
if (args.length < 5) {
  WScript.StdErr.WriteLine('usage: run_with_timeout.js <timeout_ms> <working_dir> <stdout_file> <stderr_file> <exe> [args...]');
  WScript.Quit(2);
}

function quote(s) {
  if (s.indexOf('"') >= 0) {
    s = s.replace(/"/g, '""');
  }
  if (/[\s"]/.test(s)) {
    return '"' + s + '"';
  }
  return s;
}

var timeoutMs = parseInt(args.Item(0), 10);
var workingDir = args.Item(1);
var stdoutPath = args.Item(2);
var stderrPath = args.Item(3);
var exePath = args.Item(4);
var cmd = quote(exePath);
for (var i = 5; i < args.length; i++) {
  cmd += ' ' + quote(args.Item(i));
}

var shell = new ActiveXObject('WScript.Shell');
shell.CurrentDirectory = workingDir;
var exec = shell.Exec(cmd);
var start = new Date().getTime();
while (exec.Status === 0) {
  if ((new Date().getTime() - start) >= timeoutMs) {
    exec.Terminate();
    var fsoTimeout = new ActiveXObject('Scripting.FileSystemObject');
    var outTimeout = fsoTimeout.CreateTextFile(stdoutPath, true);
    outTimeout.Write(exec.StdOut.ReadAll());
    outTimeout.Close();
    var errTimeout = fsoTimeout.CreateTextFile(stderrPath, true);
    errTimeout.Write(exec.StdErr.ReadAll());
    errTimeout.Close();
    WScript.Quit(124);
  }
  WScript.Sleep(50);
}

var fso = new ActiveXObject('Scripting.FileSystemObject');
var outFile = fso.CreateTextFile(stdoutPath, true);
outFile.Write(exec.StdOut.ReadAll());
outFile.Close();
var errFile = fso.CreateTextFile(stderrPath, true);
errFile.Write(exec.StdErr.ReadAll());
errFile.Close();
WScript.Quit(exec.ExitCode);
