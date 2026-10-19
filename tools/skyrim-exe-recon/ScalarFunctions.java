// Args: <out.tsv> <hex scalar>... Writes "scalar\tfunction_rva\tinstruction_rva" for every use.
import ghidra.app.script.GhidraScript;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.Instruction;
import ghidra.program.model.scalar.Scalar;
import java.io.PrintWriter;
import java.util.HashMap;
import java.util.Map;

public class ScalarFunctions extends GhidraScript {
    @Override
    public void run() throws Exception {
        String[] args = getScriptArgs();
        Map<Long, String> wanted = new HashMap<>();
        for (int i = 1; i < args.length; i++) {
            wanted.put(Long.parseUnsignedLong(args[i], 16), args[i]);
        }
        long base = currentProgram.getImageBase().getOffset();
        try (PrintWriter out = new PrintWriter(args[0], "UTF-8")) {
            for (Instruction ins : currentProgram.getListing().getInstructions(true)) {
                for (int op = 0; op < ins.getNumOperands(); op++) {
                    for (Object o : ins.getOpObjects(op)) {
                        if (o instanceof Scalar s && wanted.containsKey(s.getUnsignedValue())) {
                            Function f = getFunctionContaining(ins.getAddress());
                            out.printf("%s\t%x\t%x%n", wanted.get(s.getUnsignedValue()),
                                f == null ? -1 : f.getEntryPoint().getOffset() - base,
                                ins.getAddress().getOffset() - base);
                        }
                    }
                }
            }
        }
    }
}
