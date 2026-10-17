-- The absence bound relies on codepoint equality. Refuse an unsupported
-- nondeterministic projection collation instead of changing retrieval behavior.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_attribute a JOIN pg_collation c ON c.oid=a.attcollation
        WHERE a.attrelid='matching.normalized_assertion'::regclass
          AND a.attname='normalized_key' AND NOT a.attisdropped
          AND c.collisdeterministic
    ) THEN RAISE EXCEPTION 'NAME_EDIT_GUARD_REQUIRES_DETERMINISTIC_KEY_COLLATION'; END IF;
END $$;

-- Exact-semantics short circuit for existing bounded OSA retrieval helper.
-- Preserve function OID, signature, ACL, NULL behavior, volatility and search_path.
-- No score, threshold, candidate cap, source assertion or identity changes.
CREATE OR REPLACE FUNCTION matching.name_edit_within_v1(a TEXT, b TEXT, k INTEGER)
RETURNS BOOLEAN LANGUAGE plpgsql IMMUTABLE STRICT PARALLEL SAFE
SET search_path = pg_catalog AS $$
DECLARE
    n INTEGER := char_length(a); m INTEGER := char_length(b);
    prev2 INTEGER[]; prev INTEGER[]; curr INTEGER[];
    i INTEGER; j INTEGER; inf INTEGER := k + 1;
BEGIN
    IF k NOT BETWEEN 0 AND 2 OR n NOT BETWEEN 1 AND 128 OR m NOT BETWEEN 1 AND 128
       OR abs(n-m)>k THEN RETURN FALSE; END IF;
    -- A permitted edit can remove/add at most one absent character; a
    -- transposition changes neither character set. Repeated absent characters
    -- are counted individually, so this remains a conservative lower bound.
    -- Keep the same test inside the scalar helper for token-prefix callers.
    IF char_length(translate(a,b,''))>k OR char_length(translate(b,a,''))>k
    THEN RETURN FALSE; END IF;
    prev2 := array_fill(inf, ARRAY[m+1], ARRAY[0]);
    prev := array_fill(inf, ARRAY[m+1], ARRAY[0]);
    FOR j IN 0..m LOOP prev[j] := least(j,inf); END LOOP;
    FOR i IN 1..n LOOP
        curr := array_fill(inf, ARRAY[m+1], ARRAY[0]);
        curr[0] := least(i,inf);
        FOR j IN greatest(1,i-k)..least(m,i+k) LOOP
            curr[j] := least(prev[j]+1,curr[j-1]+1,
                prev[j-1]+CASE WHEN substr(a,i,1)=substr(b,j,1) THEN 0 ELSE 1 END);
            IF i>1 AND j>1 AND substr(a,i,1)=substr(b,j-1,1) AND substr(a,i-1,1)=substr(b,j,1)
            THEN curr[j] := least(curr[j],prev2[j-2]+1); END IF;
        END LOOP;
        prev2 := prev; prev := curr;
    END LOOP;
    RETURN prev[m]<=k;
END $$;

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version=31, applied_at=now()
    WHERE singleton=TRUE AND version=30;
    GET DIAGNOSTICS updated_rows=ROW_COUNT;
    IF updated_rows<>1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 30 before applying name edit guard';
    END IF;
END $$;
