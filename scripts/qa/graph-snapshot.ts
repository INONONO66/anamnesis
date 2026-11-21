export const IDENTITY_BACKGROUND_LABELS = ["Account", "AccountSession", "Person", "IdentityState"] as const;

const identityBackgroundLabels = `[${IDENTITY_BACKGROUND_LABELS.map(label => `'${label}'`).join(",")}]`;

const outsideIdentityBackground = (node: string) =>
  `NONE(label IN labels(${node}) WHERE label IN ${identityBackgroundLabels})`;

export const NON_IDENTITY_GRAPH_SNAPSHOT_QUERY = `MATCH (n)
  WHERE ${outsideIdentityBackground("n")}
  OPTIONAL MATCH (n)-[r]->(target)
  WHERE r IS NULL OR ${outsideIdentityBackground("target")}
  WITH n,r,target ORDER BY elementId(r)
  WITH n,collect({id:elementId(r),type:type(r),target:elementId(target),props:properties(r)}) AS edges
  RETURN elementId(n) AS id,labels(n) AS labels,properties(n) AS props,edges ORDER BY id`;

export const NON_IDENTITY_GRAPH_NODES_QUERY = `MATCH (n)
  WHERE ${outsideIdentityBackground("n")}
  RETURN elementId(n) AS identity,labels(n) AS labels,properties(n) AS props ORDER BY identity`;

export const NON_IDENTITY_GRAPH_EDGES_QUERY = `MATCH (a)-[r]->(b)
  WHERE ${outsideIdentityBackground("a")} AND ${outsideIdentityBackground("b")}
  RETURN elementId(r) AS identity,elementId(a) AS a,elementId(b) AS b,type(r) AS type,properties(r) AS props ORDER BY identity`;
