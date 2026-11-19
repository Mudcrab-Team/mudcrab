Scriptname MCParityInheritance extends MCParityParent

Bool hasRun = False

Event OnInit()
    If hasRun
        Return
    EndIf
    hasRun = True
    Debug.Trace("MCPARITY|inheritance|begin")
    Debug.Trace("MCPARITY|inheritance|default_vs_parent_state=" + DefaultVsParentState())
    Debug.Trace("MCPARITY|inheritance|parent_only=" + ParentOnly())
    Debug.Trace("MCPARITY|inheritance|grand_only=" + GrandOnly())
    Debug.Trace("MCPARITY|inheritance|grand_state_over_parent_default=" + GrandStateOverParentDefault())
    Debug.Trace("MCPARITY|inheritance|virtual=" + CallVirtual())
    Debug.Trace("MCPARITY|inheritance|chain=" + Chain())
    GotoState("")
    Debug.Trace("MCPARITY|inheritance|empty_state=" + DefaultVsParentState())
    Debug.Trace("MCPARITY|inheritance|done")
EndEvent

String Function DefaultVsParentState()
    Return "child.empty"
EndFunction

String Function Chain()
    Return "child/" + Parent.Chain()
EndFunction

Auto State Ready
    String Function VirtualLeaf()
        Return "child.ready"
    EndFunction
EndState
