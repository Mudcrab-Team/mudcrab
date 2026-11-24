Scriptname MCParityNestedEnd extends Quest

Bool nested = False
Bool hasRun = False

Event OnInit()
    If hasRun
        Return
    EndIf
    hasRun = True
    Debug.Trace("MCPARITY|nested_end|begin")
    GotoState("Final")
    Debug.Trace("MCPARITY|nested_end|after=" + GetState())
    Debug.Trace("MCPARITY|nested_end|done")
EndEvent

Auto State Start
    Event OnEndState()
        Debug.Trace("MCPARITY|nested_end|end.enter=" + GetState())
        If !nested
            nested = True
            GotoState("Nested")
        EndIf
        Debug.Trace("MCPARITY|nested_end|end.leave=" + GetState())
    EndEvent
EndState

State Nested
    Event OnBeginState()
        Debug.Trace("MCPARITY|nested_end|nested.begin=" + GetState())
    EndEvent
EndState

State Final
    Event OnBeginState()
        Debug.Trace("MCPARITY|nested_end|final.begin=" + GetState())
    EndEvent
EndState
