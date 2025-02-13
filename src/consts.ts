export const getRuntimeName = (tag: string): string => {
  if (tag.startsWith('Microsoft.DotNet')) {
    return 'Microsoft .NET Runtime';
  }
  return tag;
};
